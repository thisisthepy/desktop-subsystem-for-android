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
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::window_sizing::{density_for_dpi, min_client_size, rescale};
use dsa_host_core::compositor::FakeRenderer;
use dsa_host_core::host_api::{HostApiService, ServiceFacts, UserAndroid, UserAndroids};
use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids, RunningService};
use dsa_host_core::ime::{ImeCursor, ImeEditor, ImeEvent, InputKind};
use dsa_host_core::input::{buttons, PointerAction, PointerEvent, PointerTool};
use dsa_host_core::keys::{keycode, meta, KeyAction, KeyEvent};
use dsa_host_core::settings::MemorySettings;
use dsa_host_core::tasks::{AppInfo, Bounds, Task, TaskEvent, TaskRequest};
use dsa_host_windows::app_window::window::OpenWindow;
use dsa_host_windows::app_window::{self, HostSetup};
use dsa_host_windows::compositor::adapter::{DisplayPresenter, DisplaySink};
use dsa_host_windows::compositor::hosts::HostLauncher;
use dsa_host_windows::compositor::tasks::serve_input;
use dsa_host_windows::compositor::touch_gestures::TouchGestureSetup;
use dsa_host_windows::compositor::{Compositor, CompositorConfig};
use dsa_transport::local::{self, MemoryLocalConnector, Peer};
use dsa_transport::memory::{self, MemoryConnector};
use dsa_transport::{Connection, Connector, Listener, TransportError};
use windows_sys::Win32::Foundation::{HWND, POINT, RECT};
use windows_sys::Win32::Graphics::Gdi::{
    ClientToScreen, GetMonitorInfoW, MonitorFromWindow, MONITORINFO, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    VK_A, VK_BACK, VK_CONTROL, VK_MENU, VK_OEM_1, VK_RETURN, VK_RIGHT, VK_SHIFT, VK_TAB,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowRect, GetWindowTextW, GetWindowThreadProcessId, IsWindow,
    IsWindowVisible, IsZoomed, PostMessageW, SendMessageW, SetWindowPos, ShowWindow, SWP_NOMOVE,
    SWP_NOZORDER, SW_MAXIMIZE, SW_MINIMIZE, SW_RESTORE, WM_CHAR, WM_CLOSE, WM_ENTERSIZEMOVE,
    WM_EXITSIZEMOVE, WM_KEYDOWN, WM_KEYUP, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP,
    WM_MOUSEWHEEL, WM_NCLBUTTONDOWN, WM_NCLBUTTONUP, WM_RBUTTONDOWN, WM_RBUTTONUP, WM_SYSKEYDOWN,
    WM_SYSKEYUP,
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
            shortcut: None,
            connector: Box::new(self.connector.clone()),
            input: Box::new(AsUser(self.api.clone())),
            process_id: std::process::id(),
            logger: None,
            listing,
            accessibility: None,
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
    /// A way into the local API kept until the service has stopped, so the service's
    /// listener never sees its last client go while it still serves (it would say so).
    _api: MemoryLocalConnector,
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
    start_with(
        presenter.map(|presenter| presenter as Arc<dyn DisplayPresenter>),
        None,
    )
}

/// [`start`], with Windows' touch gestures turned off through `touch_gestures` while a task
/// window is in front.
fn start_with(
    presenter: Option<Arc<dyn DisplayPresenter>>,
    touch_gestures: Option<TouchGestureSetup>,
) -> Running {
    let (api_listener, api_connector) = local::pair();
    let androids = Arc::new(FakeAndroids::default());
    // The touch gestures follow the user's setting, which the service keeps.
    let service = if touch_gestures.is_some() {
        let service = HostApiService::new(androids.clone(), "test service")
            .with_settings(Arc::new(MemorySettings::default()), ServiceFacts::default());
        RunningService::start_service(Box::new(api_listener), service)
    } else {
        RunningService::start(Box::new(api_listener), androids.clone())
    };
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
    let kept_api = api_connector.clone();
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
    if let Some(setup) = touch_gestures {
        compositor = compositor.with_touch_gestures(setup);
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
        _api: kept_api,
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
        .zip([("Clock", (400u32, 300u32)), ("Maps", (500, 350))])
    {
        assert_eq!(title(window.hwnd), label);
        assert_ne!(unsafe { IsWindowVisible(window.hwnd as HWND) }, 0);
        // The task's size, or the smallest a window may have on this monitor when the task
        // is smaller: at 200 % that is 440 pixels a side.
        let size = min_client_size(dpi_of_new_windows(), None, Some(size));
        assert_eq!(client_size(window.hwnd), (size.0 as i32, size.1 as i32));
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
        ..Default::default()
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

/// The 256-pixel PNG of the DSA icon, standing in for an app's launcher icon.
fn logo_png() -> Vec<u8> {
    let icon = include_bytes!("../installer/setup-ui/assets/logo.ico");
    dsa_exe_resources::parse_ico(icon)
        .unwrap()
        .into_iter()
        .find(|image| image.width == 0)
        .unwrap()
        .data
}

/// The icon the window shows as its `kind` (ICON_SMALL or ICON_BIG) icon, as the taskbar and
/// Alt+Tab ask for it; 0 when it was given none.
fn window_icon(hwnd: isize, kind: u32) -> isize {
    use windows_sys::Win32::UI::WindowsAndMessaging::WM_GETICON;
    unsafe { SendMessageW(hwnd as HWND, WM_GETICON, kind as usize, 0) }
}

/// The width of the icon `icon`, in pixels.
fn icon_width(icon: isize) -> i32 {
    use windows_sys::Win32::Graphics::Gdi::{DeleteObject, GetObjectW, BITMAP};
    use windows_sys::Win32::UI::WindowsAndMessaging::{GetIconInfo, ICONINFO};
    unsafe {
        let mut info: ICONINFO = std::mem::zeroed();
        assert_ne!(GetIconInfo(icon as _, &mut info), 0);
        let mut bitmap: BITMAP = std::mem::zeroed();
        GetObjectW(
            info.hbmColor,
            std::mem::size_of::<BITMAP>() as i32,
            (&mut bitmap as *mut BITMAP).cast(),
        );
        DeleteObject(info.hbmColor);
        DeleteObject(info.hbmMask);
        bitmap.bmWidth
    }
}

#[test]
fn fr7_an_open_window_takes_its_apps_new_icon_at_once() {
    use dsa_host_windows::app_window::live_icon::icon_sides;
    use windows_sys::Win32::UI::WindowsAndMessaging::{ICON_BIG, ICON_SMALL};
    let running = start(None);
    // The guest could not draw the app's icon when its window host was started.
    let mut clock = AppInfo {
        package_name: "com.example.app1".into(),
        label: "Clock".into(),
        ..Default::default()
    };
    running.android.publish(TaskEvent::App(clock.clone()));
    running
        .android
        .publish(TaskEvent::Updated(task(1, "Clock", 400, 300)));
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd;
    assert_eq!(window_icon(hwnd, ICON_BIG), 0, "only its program's icon");
    // It can now: the open window shows it at once, at the sizes of its monitor's scale.
    clock.icon_png = logo_png();
    running.android.publish(TaskEvent::App(clock));
    let big = wait_for("the window to show the app's new icon", || {
        let icon = window_icon(hwnd, ICON_BIG);
        (icon != 0).then_some(icon)
    });
    let small = window_icon(hwnd, ICON_SMALL);
    assert_ne!(small, 0);
    let (small_side, big_side) = icon_sides(unsafe { GetDpiForWindow(hwnd as HWND) }.max(96));
    assert_eq!(icon_width(big), big_side as i32);
    assert_eq!(icon_width(small), small_side as i32);
    // The same window host's next window takes it as it opens.
    let mut second = task(2, "Clock 2", 400, 300);
    second.package_name = "com.example.app1".into();
    running.android.publish(TaskEvent::Updated(second));
    let open = windows_of(&running, 2);
    assert_ne!(window_icon(open[1].hwnd, ICON_BIG), 0);
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
    // meanwhile keeps the window at the size of the frame it shows.
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

/// Drags the window one step to `outer` and returns the client size the step proposes, once
/// the guest has been asked for it. The step itself does not wait for the guest: the window
/// keeps the size of the frame it shows, so the step returns at once with the old size.
fn drag_step(running: &Running, hwnd: isize, outer: (i32, i32)) -> (u32, u32) {
    let before = client_size(hwnd);
    let (window, _) = window_and_work_area(hwnd);
    let client = (
        outer.0 - (window.right - window.left - before.0),
        outer.1 - (window.bottom - window.top - before.1),
    );
    let size = (client.0 as u32, client.1 as u32);
    let started = Instant::now();
    resize_to(hwnd, outer.0, outer.1);
    let took = started.elapsed();
    assert!(
        took < Duration::from_millis(400),
        "the drag step waited {took:?} for the guest"
    );
    assert_eq!(
        client_size(hwnd),
        before,
        "the window took the size {size:?} before a frame of that size was shown"
    );
    wait_for(&format!("the guest to be asked for {size:?}"), || {
        running
            .android
            .requests
            .lock()
            .unwrap()
            .iter()
            .any(|request| {
                matches!(request, TaskRequest::Resize { task_id: 1, width, height, .. }
                    if (*width, *height) == size)
            })
            .then_some(())
    });
    size
}

#[test]
fn fr4_a_dragged_window_keeps_a_new_size_back_until_the_frame_of_that_size_is_shown() {
    let presenter = Arc::new(CapturingPresenter::default());
    let running = start(Some(Arc::clone(&presenter)));
    let Some(sink) = presenter.sink.lock().unwrap().clone() else {
        eprintln!("skipping: no Direct3D 11 device on this machine");
        return;
    };
    // Running, so the guest records the sizes it is asked for; at the monitor's density, so
    // they are the window's client sizes.
    running.android.start();
    running
        .android
        .publish(TaskEvent::Updated(task_at_monitor_density(1, 400, 300)));
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

    // Every size stays within the guest's 1280x720 buffers, the most a frame can be.
    // The guest is asked for the size while the window keeps its old one; the window takes
    // the size together with the frame of that size.
    let size = drag_step(&running, hwnd, (760, 640));
    sink.frame(3, size);
    wait_for("the window to take the size of its frame", || {
        (client_size(hwnd) == (size.0 as i32, size.1 as i32)).then_some(())
    });

    // A guest that never answers leaves the window at the size of the frame it shows.
    let shown = size;
    let late = drag_step(&running, hwnd, (720, 600));
    drag_step(&running, hwnd, (700, 580));
    thread::sleep(Duration::from_millis(300));
    assert_eq!(
        client_size(hwnd),
        (shown.0 as i32, shown.1 as i32),
        "the window changed size without a frame of that size"
    );

    // A late frame of an earlier step is still a frame of a size the window can take.
    sink.frame(3, late);
    wait_for("the window to take the size of the late frame", || {
        (client_size(hwnd) == (late.0 as i32, late.1 as i32)).then_some(())
    });

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

/// The DPI a new top-level window gets on this machine. The window hosts are per-monitor
/// aware, so the DPI is read as such a thread sees it: a test thread that is not aware is
/// told 96 whatever the monitor's scale.
fn dpi_of_new_windows() -> u32 {
    use windows_sys::Win32::UI::HiDpi::{
        GetDpiForSystem, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let dpi = GetDpiForSystem();
        SetThreadDpiAwarenessContext(previous);
        dpi.max(96)
    }
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

/// Checks that `hwnd` is maximised with its client area exactly on its monitor's work area:
/// not the whole monitor, under the taskbar, and not its restored size. `when` says which step
/// of the test it is.
fn assert_fills_its_work_area(hwnd: isize, when: &str) {
    assert_ne!(
        unsafe { IsZoomed(hwnd as HWND) },
        0,
        "the window is no longer maximised {when}"
    );
    let (window, work) = window_and_work_area(hwnd);
    let mut origin = POINT { x: 0, y: 0 };
    unsafe { ClientToScreen(hwnd as HWND, &mut origin) };
    let (width, height) = client_size(hwnd);
    let client = (origin.x, origin.y, origin.x + width, origin.y + height);
    assert_eq!(
        client,
        (work.left, work.top, work.right, work.bottom),
        "{when}, the maximised window at {}x{}..{}x{} has its client area at {client:?}, not on the work area",
        window.left,
        window.top,
        window.right,
        window.bottom,
    );
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
        // A frame of the window's size, which may be larger than the task's on a monitor
        // whose smallest window is larger.
        let (width, height) = client_size(hwnd);
        sink.frame(3, (width as u32, height as u32));
        handle
    });

    // Only the requests the maximise makes count, not the ones the window made opening.
    thread::sleep(Duration::from_millis(200));
    let before = running.android.requests.lock().unwrap().len();
    let normal = client_size(hwnd);
    unsafe { ShowWindow(hwnd as HWND, SW_MAXIMIZE) };
    assert_ne!(unsafe { IsZoomed(hwnd as HWND) }, 0);
    // The task is given the size live, so its app lays itself out again instead of being
    // recreated at a size it has not drawn at.
    wait_for("the maximised size to reach the guest", || {
        (resizes_since(&running, before, 1) == vec![(max, true)]).then_some(())
    });
    // A maximised window holds the size the desktop gives it, its own monitor's work area,
    // at once: the guest's frame, smaller than that, is shown in its middle 1:1, in the same
    // texture, and the window does not shrink to it.
    assert_fills_its_work_area(hwnd, "maximised");
    if let (Some(sink), Some(texture)) = (&sink, texture) {
        assert_eq!(sink.target(3, 1280, 720), Some(texture), "a new texture");
        sink.frame(3, max);
        thread::sleep(Duration::from_millis(300));
        assert_fills_its_work_area(hwnd, "after a frame of the guest's largest size");
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
    // The window stays maximised on its work area, and the guest is asked for nothing larger
    // than it draws, nor of size zero.
    thread::sleep(Duration::from_millis(300));
    assert_fills_its_work_area(hwnd, "after the task was reconfigured");
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
    let restored = (normal.0 as u32, normal.1 as u32);
    assert_ne!(restored, max);
    wait_for("the restored size to reach the guest", || {
        (resizes_since(&running, restored_from, 1) == vec![(restored, true)]).then_some(())
    });
    if let Some(sink) = &sink {
        sink.frame(3, restored);
    }
    wait_for("the window to take its restored size", || {
        (client_size(hwnd) == normal).then_some(())
    });

    running.android.publish(TaskEvent::Removed { task_id: 1 });
    windows_of(&running, 0);
}

#[test]
fn fr4_a_window_maximised_with_a_guest_larger_than_its_monitor_fills_the_work_area_and_restores() {
    let presenter = Arc::new(CapturingPresenter::default());
    let running = start(Some(Arc::clone(&presenter)));
    running.android.start();
    // The guest draws more than any monitor shows, as a 2880x2880 guest display does on a
    // 2048x1152 monitor.
    let mut notes = task_at_monitor_density(1, 400, 300);
    notes.max_width = 8000;
    notes.max_height = 8000;
    running.android.publish(TaskEvent::Updated(notes.clone()));
    running.android.publish(TaskEvent::Frames {
        task_id: 1,
        display_id: 3,
    });
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd;
    let (_, work) = window_and_work_area(hwnd);
    let filled = (
        (work.right - work.left) as u32,
        (work.bottom - work.top) as u32,
    );
    let sink = presenter.sink.lock().unwrap().clone();
    if let Some(sink) = &sink {
        wait_for("display 3 to reach task 1's window", || {
            sink.target(3, filled.0.max(1280), filled.1.max(720))
        });
        let (width, height) = client_size(hwnd);
        sink.frame(3, (width as u32, height as u32));
    }

    thread::sleep(Duration::from_millis(200));
    let before = running.android.requests.lock().unwrap().len();
    let normal = client_size(hwnd);
    unsafe { ShowWindow(hwnd as HWND, SW_MAXIMIZE) };
    assert_ne!(unsafe { IsZoomed(hwnd as HWND) }, 0);
    // The guest is asked for the work area of the window's own monitor, never more.
    wait_for("the maximised size to reach the guest", || {
        (resizes_since(&running, before, 1) == vec![(filled, true)]).then_some(())
    });
    if let Some(sink) = &sink {
        sink.frame(3, filled);
        wait_for("the window to take the size of its frame", || {
            (client_size(hwnd) == (filled.0 as i32, filled.1 as i32)).then_some(())
        });
    }
    // Its client area is the work area, on the screen, and stays there once the frame of that
    // size is shown and the guest has had time to answer.
    assert_fills_its_work_area(hwnd, "maximised");
    thread::sleep(Duration::from_millis(1500));
    assert_fills_its_work_area(hwnd, "1.5 s after the frame of its size");
    let asked = resizes_since(&running, before, 1);
    assert!(
        asked
            .iter()
            .all(|((width, height), _)| *width <= filled.0 && *height <= filled.1),
        "a size larger than the work area {}x{} was asked for: {asked:?}",
        filled.0,
        filled.1
    );

    // Restoring it gives the window and the task their old size back.
    let mut maximised = notes.clone();
    maximised.bounds.right = maximised.bounds.left + filled.0 as i32;
    maximised.bounds.bottom = maximised.bounds.top + filled.1 as i32;
    running.android.publish(TaskEvent::Updated(maximised));
    let restored_from = running.android.requests.lock().unwrap().len();
    unsafe { ShowWindow(hwnd as HWND, SW_RESTORE) };
    assert_eq!(unsafe { IsZoomed(hwnd as HWND) }, 0);
    let restored = (normal.0 as u32, normal.1 as u32);
    wait_for("the restored size to reach the guest", || {
        resizes_since(&running, restored_from, 1)
            .contains(&(restored, true))
            .then_some(())
    });
    if let Some(sink) = &sink {
        sink.frame(3, restored);
        wait_for("the window to take its restored size", || {
            (client_size(hwnd) == normal).then_some(())
        });
    }

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
    notes.density_dpi = 130;
    notes.max_width = 1920;
    notes.max_height = 1080;
    running.android.publish(TaskEvent::Updated(notes));
    let open = windows_of(&running, 1);
    let dpi = unsafe { GetDpiForWindow(open[0].hwnd as HWND) }.max(96);
    let wanted = density_for_dpi(dpi);

    let (width, height) = rescale((400, 300), 130, wanted);
    assert_eq!(client_size(open[0].hwnd), (width as i32, height as i32));
    if wanted != 130 {
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

/// A renderer whose Vulkan device the test loses and makes again, as a GPU reset does.
#[derive(Default)]
struct ResettingPresenter {
    generation: AtomicU64,
}

impl DisplayPresenter for ResettingPresenter {
    fn present_displays(&self, _: Option<Arc<dyn DisplaySink>>) -> Result<(), String> {
        Ok(())
    }
    fn adapter_luid(&self) -> Option<[u8; 8]> {
        None
    }
    fn device_generation(&self) -> u64 {
        self.generation.load(Ordering::SeqCst)
    }
}

/// The requests the fake Android was given from the `skip`th on.
fn requests_since(running: &Running, skip: usize) -> Vec<TaskRequest> {
    running.android.requests.lock().unwrap()[skip..].to_vec()
}

#[test]
fn nfr5_every_input_of_a_window_taken_over_after_a_gpu_reset_reaches_the_new_task() {
    let presenter = Arc::new(ResettingPresenter::default());
    let running = start_with(
        Some(Arc::clone(&presenter) as Arc<dyn DisplayPresenter>),
        None,
    );
    running.android.start();
    let app = "com.example.app9";
    running
        .android
        .publish(TaskEvent::Updated(task_at_monitor_density(9, 400, 300)));
    let open = windows_of(&running, 1);
    let hwnd = open[0].hwnd;

    // A GPU reset, which the window manager sees at its next look; then the app's task ends
    // because of it, and the window stays for the app's next task.
    presenter.generation.fetch_add(1, Ordering::SeqCst);
    thread::sleep(Duration::from_secs(2));
    running.android.publish(TaskEvent::Removed { task_id: 9 });
    wait_for("the app to be started again", || {
        running
            .android
            .requests
            .lock()
            .unwrap()
            .contains(&TaskRequest::Launch {
                package_name: app.into(),
            })
            .then_some(())
    });
    assert_eq!(windows_of(&running, 1)[0].hwnd, hwnd);

    // The app's new task takes the window over. Its title follows the window host's word on
    // which task the window shows, on the same connection, so once the title is there every
    // input after it goes to the new task.
    let mut again = task_at_monitor_density(14, 400, 300);
    again.package_name = app.into();
    again.label = "Notes again".into();
    again.can_go_back = true;
    running.android.publish(TaskEvent::Updated(again));
    wait_for("the window to show the new task's title", || {
        (title(hwnd) == "Notes again").then_some(())
    });
    let open = running.windows();
    assert_eq!(open.len(), 1);
    assert_eq!(open[0].hwnd, hwnd);
    let window = hwnd as HWND;

    // The mouse and the wheel.
    let (width, height) = client_size(hwnd);
    let (x, y) = (width / 2, height / 2);
    let mut point = POINT { x, y };
    unsafe { ClientToScreen(window, &mut point) };
    unsafe {
        PostMessageW(window, WM_LBUTTONDOWN, MK_LBUTTON as usize, lparam(x, y));
        PostMessageW(window, WM_LBUTTONUP, 0, lparam(x, y));
        PostMessageW(
            window,
            WM_MOUSEWHEEL,
            ((-120i16) as u16 as usize) << 16,
            lparam(point.x, point.y),
        );
    }
    let given: Vec<(i32, PointerAction)> = pointers(&running, 3)
        .iter()
        .map(|event| (event.task_id, event.action))
        .collect();
    assert_eq!(
        given,
        vec![
            (14, PointerAction::Down),
            (14, PointerAction::Up),
            (14, PointerAction::Scroll)
        ]
    );

    // A key, and a letter typed as text.
    send_key(window, WM_KEYDOWN, VK_RETURN, 0x1c, false);
    send_key(window, WM_KEYUP, VK_RETURN, 0x1c, false);
    unsafe {
        PostMessageW(
            window,
            WM_KEYDOWN,
            VK_A as usize,
            key_lparam(0x1e, false, true),
        );
        PostMessageW(
            window,
            WM_KEYUP,
            VK_A as usize,
            key_lparam(0x1e, false, false),
        );
    }
    assert_eq!(
        text(&running, 1),
        vec![ImeEvent::Commit {
            task_id: 14,
            text: "a".into()
        }]
    );

    // The title bar's back arrow, which the new task shows since it can go back.
    use dsa_host_windows::app_window::caption::HT_BACK;
    unsafe {
        PostMessageW(window, WM_NCLBUTTONDOWN, HT_BACK as usize, 0);
        PostMessageW(window, WM_NCLBUTTONUP, HT_BACK as usize, 0);
    }
    let sent: Vec<(i32, KeyAction, u32)> = keys(&running, 4)
        .iter()
        .map(|event| (event.task_id, event.action, event.key_code))
        .collect();
    assert_eq!(
        sent,
        vec![
            (14, KeyAction::Down, keycode::ENTER),
            (14, KeyAction::Up, keycode::ENTER),
            (14, KeyAction::Down, keycode::BACK),
            (14, KeyAction::Up, keycode::BACK),
        ]
    );

    // A resize, a maximise and a close are asked of the new task.
    resize_to(hwnd, 640, 520);
    let (width, height) = client_size(hwnd);
    wait_for("the new size to reach the new task", || {
        (last_resize(&running, 14) == Some((width as u32, height as u32))).then_some(())
    });
    let before = running.android.requests.lock().unwrap().len();
    unsafe { ShowWindow(window, SW_MAXIMIZE) };
    wait_for("the maximised size to reach Android", || {
        requests_since(&running, before)
            .iter()
            .any(|request| matches!(request, TaskRequest::Resize { .. }))
            .then_some(())
    });
    unsafe { PostMessageW(window, WM_CLOSE, 0, 0) };
    wait_for("the close to reach Android", || {
        requests_since(&running, before)
            .contains(&TaskRequest::Close { task_id: 14 })
            .then_some(())
    });
    // Nothing went to the task that ended.
    let all = running.android.requests.lock().unwrap().clone();
    let launch = all
        .iter()
        .position(|request| {
            *request
                == TaskRequest::Launch {
                    package_name: app.into(),
                }
        })
        .unwrap();
    for request in &all[launch..] {
        let named = match request {
            TaskRequest::Resize { task_id, .. } | TaskRequest::Close { task_id } => Some(*task_id),
            _ => None,
        };
        assert_ne!(named, Some(9), "{request:?} went to the task that ended");
    }
    // A hover can come from the machine's own cursor resting over the window before the
    // reset; everything the test did went to the new task.
    let pointed: Vec<PointerEvent> = running
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
    assert!(
        pointed.iter().all(|event| event.task_id == 14),
        "{pointed:?}"
    );
    let typed = running.android.text.lock().unwrap().clone();
    assert!(typed.iter().all(|event| event.task_id() == 14), "{typed:?}");
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
    // The window shows the task 1:1, so the point is the same pixel of the task, kept
    // inside it while the window is larger (the smallest window can exceed a small task).
    let expected_x = x.min(399) as f32;
    let expected_y = y.min(299) as f32;
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
    // The window shows the task 1:1: the wheel scrolls at the same pixel of the task.
    assert!((scroll.x - 10.0).abs() <= 2.0, "{scroll:?}");
    assert!((scroll.y - 20.0).abs() <= 2.0, "{scroll:?}");
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
fn fr6_the_window_reads_the_editor_the_guest_describes_and_types_into_it() {
    let running = start(None);
    let hwnd = one_task_window(&running);
    let editor = ImeEditor {
        kind: InputKind::Email,
        text: "me@".into(),
        text_start: 0,
        selection: (3, 3),
        composition: None,
    };
    running.android.publish(TaskEvent::ImeEditor {
        task_id: 7,
        editor: editor.clone(),
    });
    wait_for("the window to read the editor", || {
        (running.windows().first()?.ime_editor.as_ref() == Some(&editor)).then_some(())
    });
    // A character typed without an input method goes at the cursor, once.
    unsafe {
        SendMessageW(hwnd, WM_CHAR, usize::from(b'z'), 1);
    }
    assert_eq!(text(&running, 1), vec![commit("z")]);
    thread::sleep(Duration::from_millis(200));
    assert_eq!(running.android.text.lock().unwrap().len(), 1);
}

#[test]
fn fr6_a_character_typed_where_no_editor_takes_text_still_reaches_the_task() {
    let running = start(None);
    let hwnd = one_task_window(&running);
    running.android.publish(TaskEvent::ImeEditor {
        task_id: 7,
        editor: ImeEditor::none(),
    });
    wait_for("the window to read that no editor takes text", || {
        let window = running.windows().first()?.clone();
        (window.ime_editor?.kind == InputKind::None).then_some(())
    });
    // A game reads it as the keys that type it, which the agent finds for the text.
    unsafe {
        SendMessageW(hwnd, WM_CHAR, usize::from(b'w'), 1);
    }
    assert_eq!(text(&running, 1), vec![commit("w")]);
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

/// Fingers and a pen put into the machine's real input with Windows' synthetic pointer
/// devices, so they reach a task window the way a touchscreen's and a pen's do: through the
/// window's hit test and its pointer messages, not as messages posted to it.
///
/// They move the machine's real input, so they are ignored unless asked for, and the
/// Installer test workflow asks for them on its Windows runners, where nobody is using the
/// desktop.
mod real_input {
    use super::*;
    use dsa_host_windows::compositor::touch_gestures::{GestureSetting, SystemGestures};
    use std::ffi::c_void;
    use windows_sys::Win32::Foundation::CloseHandle;
    use windows_sys::Win32::System::Threading::{
        OpenProcess, QueryFullProcessImageNameW, TerminateProcess,
        PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        keybd_event, KEYEVENTF_KEYUP, VK_ESCAPE,
    };
    use windows_sys::Win32::UI::Input::Pointer::{
        POINTER_FLAG_DOWN, POINTER_FLAG_INCONTACT, POINTER_FLAG_INRANGE, POINTER_FLAG_UP,
        POINTER_FLAG_UPDATE, POINTER_PEN_INFO, POINTER_TOUCH_INFO,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetAncestor, GetClassNameW, IsIconic, WindowFromPoint, GA_ROOT, HTMINBUTTON, HWND_TOPMOST,
        PEN_FLAG_BARREL, PEN_FLAG_ERASER, PEN_FLAG_INVERTED, PEN_MASK_PRESSURE, PEN_MASK_TILT_X,
        PEN_MASK_TILT_Y, POINTER_INPUT_TYPE, PT_PEN, PT_TOUCH, SWP_NOSIZE, TOUCH_MASK_CONTACTAREA,
        TOUCH_MASK_PRESSURE, WM_NCHITTEST,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        GetForegroundWindow, SetForegroundWindow, SystemParametersInfoW,
    };

    /// One contact of a synthetic pointer device's frame (`POINTER_TYPE_INFO`).
    #[repr(C)]
    #[derive(Clone, Copy)]
    union Contact {
        touch: POINTER_TOUCH_INFO,
        pen: POINTER_PEN_INFO,
    }

    #[repr(C)]
    #[derive(Clone, Copy)]
    struct PointerTypeInfo {
        kind: POINTER_INPUT_TYPE,
        contact: Contact,
    }

    /// `POINTER_FEEDBACK_NONE`: Windows draws no touch or pen feedback for the contacts.
    const NO_FEEDBACK: i32 = 3;

    // user32's synthetic pointer devices, declared as user32 exports them, so the tests need
    // no more of windows-sys than the pointer structures.
    #[link(name = "user32")]
    extern "system" {
        fn CreateSyntheticPointerDevice(
            kind: POINTER_INPUT_TYPE,
            max_count: u32,
            mode: i32,
        ) -> *mut c_void;
        fn InjectSyntheticPointerInput(
            device: *mut c_void,
            info: *const PointerTypeInfo,
            count: u32,
        ) -> i32;
        fn DestroySyntheticPointerDevice(device: *mut c_void);
    }

    /// A synthetic touchscreen or pen, gone when dropped.
    struct Device(*mut c_void);

    impl Device {
        fn new(kind: POINTER_INPUT_TYPE, contacts: u32) -> Device {
            // SAFETY: plain values; the device is destroyed when dropped.
            let device = unsafe { CreateSyntheticPointerDevice(kind, contacts, NO_FEEDBACK) };
            assert!(
                !device.is_null(),
                "Windows would not make a synthetic pointer device of type {kind} on this \
                 machine: {}",
                std::io::Error::last_os_error()
            );
            Device(device)
        }

        /// Puts one frame of contacts into the machine's input, and gives Windows a moment
        /// to deliver it, as a digitizer's next frame would come.
        fn inject(&self, frame: &[PointerTypeInfo]) {
            // SAFETY: the frame outlives the call and holds `frame.len()` contacts.
            let injected =
                unsafe { InjectSyntheticPointerInput(self.0, frame.as_ptr(), frame.len() as u32) };
            assert_ne!(
                injected,
                0,
                "Windows refused synthetic pointer input: {}",
                std::io::Error::last_os_error()
            );
            thread::sleep(Duration::from_millis(16));
        }
    }

    impl Drop for Device {
        fn drop(&mut self) {
            // SAFETY: made by CreateSyntheticPointerDevice and destroyed once.
            unsafe { DestroySyntheticPointerDevice(self.0) };
        }
    }

    const DOWN: u32 = POINTER_FLAG_DOWN | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT;
    const MOVING: u32 = POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE | POINTER_FLAG_INCONTACT;
    const HOVERING: u32 = POINTER_FLAG_UPDATE | POINTER_FLAG_INRANGE;
    /// A finger that lifts is gone; a pen that lifts stays in range, hovering.
    const LIFTED: u32 = POINTER_FLAG_UP;
    const PEN_LIFTED: u32 = POINTER_FLAG_UP | POINTER_FLAG_INRANGE;
    /// A pen that leaves the digitizer's range: an update that is neither in range nor in
    /// contact.
    const OUT_OF_RANGE: u32 = POINTER_FLAG_UPDATE;

    fn finger(id: u32, flags: u32, at: POINT) -> PointerTypeInfo {
        // SAFETY: plain data, for which all zero is a valid value.
        let mut touch: POINTER_TOUCH_INFO = unsafe { std::mem::zeroed() };
        touch.pointerInfo.pointerType = PT_TOUCH;
        touch.pointerInfo.pointerId = id;
        touch.pointerInfo.pointerFlags = flags;
        touch.pointerInfo.ptPixelLocation = at;
        touch.touchMask = TOUCH_MASK_CONTACTAREA | TOUCH_MASK_PRESSURE;
        touch.rcContact = RECT {
            left: at.x - 2,
            top: at.y - 2,
            right: at.x + 2,
            bottom: at.y + 2,
        };
        touch.pressure = if flags & POINTER_FLAG_INCONTACT != 0 {
            512
        } else {
            0
        };
        PointerTypeInfo {
            kind: PT_TOUCH,
            contact: Contact { touch },
        }
    }

    fn pen(flags: u32, pen_flags: u32, pressure: u32, at: POINT) -> PointerTypeInfo {
        // SAFETY: plain data, for which all zero is a valid value.
        let mut pen: POINTER_PEN_INFO = unsafe { std::mem::zeroed() };
        pen.pointerInfo.pointerType = PT_PEN;
        pen.pointerInfo.pointerFlags = flags;
        pen.pointerInfo.ptPixelLocation = at;
        pen.penFlags = pen_flags;
        pen.penMask = PEN_MASK_PRESSURE | PEN_MASK_TILT_X | PEN_MASK_TILT_Y;
        pen.pressure = pressure;
        pen.tiltX = 30;
        pen.tiltY = -20;
        PointerTypeInfo {
            kind: PT_PEN,
            contact: Contact { pen },
        }
    }

    /// A task window on top of every other window, at the top left of its monitor's work
    /// area so the whole window is on the screen, and where its client area begins on the
    /// screen, once the window is the one under `points` (client points). A point counts as
    /// on the window when the window under it, or the top-level window that holds it, is the
    /// task window.
    ///
    /// A topmost window still sits below the shell's own windows, such as the Start menu,
    /// which the ARM64 runner opens by itself. While one covers the contacts it is closed as
    /// a user would close it, with Escape, and the task window is brought to the front; one
    /// that stays is ended. The task window is made the foreground window too, as it is when
    /// a user works in it.
    fn window_on_top(running: &Running, points: &[(i32, i32)]) -> (HWND, POINT) {
        let open = windows_of(running, 1);
        let hwnd = open[0].hwnd as HWND;
        let (_, work) = window_and_work_area(open[0].hwnd);
        unsafe { SetWindowPos(hwnd, HWND_TOPMOST, work.left, work.top, 0, 0, SWP_NOSIZE) };
        let under = |origin: POINT, point: (i32, i32)| -> HWND {
            unsafe { WindowFromPoint(on_screen(origin, point)) }
        };
        let deadline = Instant::now() + WAIT;
        let mut tries = 0u32;
        let mut next_try = Instant::now();
        loop {
            let mut origin = POINT { x: 0, y: 0 };
            unsafe { ClientToScreen(hwnd, &mut origin) };
            let covers: Vec<HWND> = points
                .iter()
                .map(|&point| under(origin, point))
                .filter(|&found| found != hwnd && unsafe { GetAncestor(found, GA_ROOT) } != hwnd)
                .collect();
            let in_front = unsafe { GetForegroundWindow() } == hwnd;
            if covers.is_empty() && (in_front || tries >= 3) {
                return (hwnd, origin);
            }
            if Instant::now() >= next_try {
                tries += 1;
                next_try = Instant::now() + Duration::from_millis(500);
                bring_to_front(hwnd, covers.first().copied(), tries);
            }
            if Instant::now() >= deadline {
                let (window, work) = window_and_work_area(open[0].hwnd);
                let misses: Vec<String> = points
                    .iter()
                    .map(|&point| {
                        let at = on_screen(origin, point);
                        format!(
                            "{point:?} at ({}, {}) on the screen: {}",
                            at.x,
                            at.y,
                            describe_window(under(origin, point))
                        )
                    })
                    .collect();
                panic!(
                    "timed out waiting for the task window to be the window under the \
                     contacts: the task window {} is at ({}, {}) to ({}, {}), its client area \
                     begins at ({}, {}), the work area is ({}, {}) to ({}, {}); under the \
                     contacts: {misses:?}",
                    describe_window(hwnd),
                    window.left,
                    window.top,
                    window.right,
                    window.bottom,
                    origin.x,
                    origin.y,
                    work.left,
                    work.top,
                    work.right,
                    work.bottom,
                );
            }
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// As [`window_on_top`], and then the foreground window as well, without exception.
    /// [`window_on_top`] settles for a task window that is under the contacts but not in
    /// front after a few tries, which input tests can live with. A test of what happens while
    /// a task window is in front cannot: on the runners the shell's search pane can hold the
    /// foreground without covering the contacts. A shell window that keeps the foreground is
    /// closed with Escape and then ended, as in [`bring_to_front`].
    fn window_in_front(running: &Running, points: &[(i32, i32)]) -> (HWND, POINT) {
        let (hwnd, origin) = window_on_top(running, points);
        let deadline = Instant::now() + WAIT;
        let mut tries = 0u32;
        loop {
            let foreground = unsafe { GetForegroundWindow() };
            let root = if foreground.is_null() {
                foreground
            } else {
                unsafe { GetAncestor(foreground, GA_ROOT) }
            };
            if foreground == hwnd || root == hwnd {
                return (hwnd, origin);
            }
            assert!(
                Instant::now() < deadline,
                "timed out bringing the task window {} to the front; the foreground window is {}",
                describe_window(hwnd),
                describe_window(foreground)
            );
            tries += 1;
            // window_on_top has tried three times already, so a shell window that holds the
            // foreground now is ended unless Escape closes it.
            let holder = (!root.is_null()).then_some(root);
            bring_to_front(hwnd, holder, tries + 2);
            thread::sleep(Duration::from_millis(500));
        }
    }

    /// The shell's programs whose windows open over every other window: the Start menu,
    /// search, the notification and quick settings panes, and WWAHost, in which Windows
    /// shows its full-screen first-run pages ("Let's finish setting up your device"). On
    /// the ARM64 runner such a page covers the whole screen from sign-in, and Escape does
    /// not close it.
    const SHELL_POPUPS: &[&str] = &[
        "StartMenuExperienceHost.exe",
        "SearchHost.exe",
        "SearchApp.exe",
        "ShellExperienceHost.exe",
        "WWAHost.exe",
    ];

    /// Brings `hwnd` to the front as a user would: Escape closes a Start menu or a search
    /// pane that holds the keyboard, and an Alt tap lets this process move the foreground.
    /// From the third try on, a shell window that still covers the contacts (`cover`) is
    /// ended; Windows starts its program again when it is next needed.
    fn bring_to_front(hwnd: HWND, cover: Option<HWND>, tries: u32) {
        // SAFETY: plain calls on window handles. The keys go to the foreground window, which
        // is not the task window then, so the task gets neither key.
        unsafe {
            if GetForegroundWindow() != hwnd {
                keybd_event(VK_ESCAPE as u8, 0, 0, 0);
                keybd_event(VK_ESCAPE as u8, 0, KEYEVENTF_KEYUP, 0);
                keybd_event(VK_MENU as u8, 0, 0, 0);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
                SetForegroundWindow(hwnd);
            }
            SetWindowPos(hwnd, HWND_TOPMOST, 0, 0, 0, 0, SWP_NOSIZE | SWP_NOMOVE);
        }
        let Some(cover) = cover else {
            return;
        };
        if tries < 3 {
            return;
        }
        let (process_id, program) = program_of(cover);
        if SHELL_POPUPS
            .iter()
            .any(|name| program.eq_ignore_ascii_case(name))
        {
            eprintln!(
                "{} covers the task window's contacts; ending {program} ({process_id})",
                describe_window(cover)
            );
            // SAFETY: opens the shell process by its ID, ends it, and closes the handle.
            unsafe {
                let process = OpenProcess(PROCESS_TERMINATE, 0, process_id);
                if !process.is_null() {
                    TerminateProcess(process, 1);
                    CloseHandle(process);
                }
            }
        }
    }

    /// The ID and the program file name of the process that owns `hwnd`.
    fn program_of(hwnd: HWND) -> (u32, String) {
        let mut process_id = 0u32;
        // SAFETY: writes the owner's ID into a local.
        unsafe { GetWindowThreadProcessId(hwnd, &mut process_id) };
        let mut name = [0u16; 1024];
        let mut length = name.len() as u32;
        // SAFETY: opens the process for its name only, reads it into `name` and closes it.
        let read = unsafe {
            let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id);
            if process.is_null() {
                false
            } else {
                let read = QueryFullProcessImageNameW(process, 0, name.as_mut_ptr(), &mut length);
                CloseHandle(process);
                read != 0
            }
        };
        let path = if read {
            String::from_utf16_lossy(&name[..length as usize])
        } else {
            String::new()
        };
        let program = path.rsplit('\\').next().unwrap_or_default().to_string();
        (process_id, program)
    }

    /// A window's handle, class, title and program, for a failure's message.
    fn describe_window(hwnd: HWND) -> String {
        if hwnd.is_null() {
            return "no window".to_string();
        }
        let mut class = [0u16; 128];
        let length = unsafe { GetClassNameW(hwnd, class.as_mut_ptr(), class.len() as i32) };
        let class = String::from_utf16_lossy(&class[..length.max(0) as usize]);
        let root = unsafe { GetAncestor(hwnd, GA_ROOT) };
        let (process_id, program) = program_of(root);
        let caption = title(root as isize);
        format!("{hwnd:?} of class {class:?} titled {caption:?} of {program} ({process_id})")
    }

    fn on_screen(origin: POINT, (x, y): (i32, i32)) -> POINT {
        POINT {
            x: origin.x + x,
            y: origin.y + y,
        }
    }

    /// The pointer events the fake Android was given by `tool`, once `done` holds for them.
    fn given(
        running: &Running,
        tool: PointerTool,
        what: &str,
        done: impl Fn(&[PointerEvent]) -> bool,
    ) -> Vec<PointerEvent> {
        given_after(running, 0, tool, what, done)
    }

    /// As [`given`], from the `skip`th event the fake Android was given on.
    fn given_after(
        running: &Running,
        skip: usize,
        tool: PointerTool,
        what: &str,
        done: impl Fn(&[PointerEvent]) -> bool,
    ) -> Vec<PointerEvent> {
        let deadline = Instant::now() + WAIT;
        loop {
            let all: Vec<PointerEvent> = running.android.pointers.lock().unwrap().clone();
            let events: Vec<PointerEvent> = all
                .iter()
                .skip(skip)
                .filter(|event| event.tool == tool)
                .copied()
                .collect();
            if done(&events) {
                return events;
            }
            // What did arrive says which part of the input went missing.
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; the task was given: {all:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    /// Taps one finger at `at` (a screen point) until the task gets the tap, and returns how
    /// many events the task had been given by then.
    ///
    /// Windows can ignore touch while and just after a pen is in range ("Ignore touch input
    /// when I'm using my pen"), and the pen test runs just before this one. A tap that comes
    /// through says touch reaches the window at all, so what goes missing after it is down
    /// to the number of fingers; a tap that never comes through says touch as a whole is
    /// held back.
    fn one_finger_reaches(running: &Running, at: POINT) -> usize {
        let deadline = Instant::now() + WAIT;
        let mut taps = 0u32;
        loop {
            let start = running.android.pointers.lock().unwrap().len();
            {
                let finger_device = Device::new(PT_TOUCH, 1);
                finger_device.inject(&[finger(0, DOWN, at)]);
                finger_device.inject(&[finger(0, LIFTED, at)]);
            }
            taps += 1;
            let tap_deadline = (Instant::now() + Duration::from_secs(2)).min(deadline);
            while Instant::now() < tap_deadline {
                let all: Vec<PointerEvent> = running.android.pointers.lock().unwrap().clone();
                if all[start..].iter().any(|event| {
                    event.tool == PointerTool::Touch && event.action == PointerAction::Up
                }) {
                    eprintln!("one finger reached the task after {taps} taps");
                    return all.len();
                }
                thread::sleep(Duration::from_millis(20));
            }
            let all: Vec<PointerEvent> = running.android.pointers.lock().unwrap().clone();
            assert!(
                Instant::now() < deadline,
                "not even one finger reached the task window in {taps} taps, so Windows holds \
                 touch back as a whole; the task was given: {all:?}"
            );
        }
    }

    /// What Windows 11's three- and four-finger touch gestures read, for a failure's
    /// message. While they are on, Windows keeps three or more fingers from every app and
    /// holds even one finger back until it lifts; the Installer test workflow turns them off.
    /// winuser.h does not name the action: it is the one Settings changes them with, which
    /// the workflow's script checks against the user's TouchGestureSetting.
    fn touch_gestures() -> String {
        const GET_TOUCH_GESTURES: u32 = 0x2030;
        let mut value = 0u32;
        // SAFETY: a get action that writes one 32-bit value into a local.
        let read = unsafe {
            SystemParametersInfoW(
                GET_TOUCH_GESTURES,
                0,
                (&mut value as *mut u32).cast::<c_void>(),
                0,
            )
        };
        if read == 0 {
            format!(
                "Windows does not say whether its three- and four-finger touch gestures are on \
                 ({})",
                std::io::Error::last_os_error()
            )
        } else if value == 0 {
            "Windows' three- and four-finger touch gestures are off".to_string()
        } else {
            format!(
                "Windows' three- and four-finger touch gestures are on ({value}), so Windows \
                 keeps three or more fingers from every app"
            )
        }
    }

    fn count(events: &[PointerEvent], action: PointerAction) -> usize {
        events.iter().filter(|event| event.action == action).count()
    }

    #[test]
    #[ignore = "moves the machine's real touch input; the Installer test workflow runs it"]
    fn fr5_real_fingers_on_a_task_window_reach_its_task_as_ten_touch_pointers_and_a_pinch() {
        let running = start(None);
        running.android.start();
        running
            .android
            .publish(TaskEvent::Updated(task(7, "Paint", 800, 600)));
        let open = windows_of(&running, 1);
        let (width, height) = client_size(open[0].hwnd);
        // Ten fingers in a row across the lower half, below the title bar, which spread
        // apart as a pinch does.
        let row = height * 3 / 4;
        let at = |index: i32, spread: i32| {
            let middle = width / 2;
            (middle + (index * 2 - 9) * (width / 40 + spread), row)
        };
        let wide: Vec<(i32, i32)> = (0..10).map(|index| at(index, 6)).collect();
        let (hwnd, origin) = window_on_top(&running, &wide);
        // Which window has the keyboard says, if fingers go missing, whether the task window
        // was the one in use when they landed.
        let gestures = touch_gestures();
        eprintln!(
            "ten fingers on {}; the foreground window is {}; {gestures}",
            describe_window(hwnd),
            describe_window(unsafe { GetForegroundWindow() })
        );
        let before = one_finger_reaches(&running, on_screen(origin, at(0, 0)));

        let fingers = Device::new(PT_TOUCH, 10);
        let frame = |flags: u32, spread: i32| -> Vec<PointerTypeInfo> {
            (0..10)
                .map(|index| finger(index as u32, flags, on_screen(origin, at(index, spread))))
                .collect()
        };
        fingers.inject(&frame(DOWN, 0));
        for spread in 1..=6 {
            fingers.inject(&frame(MOVING, spread));
        }
        fingers.inject(&frame(LIFTED, 6));

        let events = given_after(
            &running,
            before,
            PointerTool::Touch,
            &format!("ten fingers to lift ({gestures})"),
            |events| count(events, PointerAction::Up) + count(events, PointerAction::Cancel) >= 10,
        );
        assert_eq!(count(&events, PointerAction::Cancel), 0, "{events:?}");
        let downs: Vec<&PointerEvent> = events
            .iter()
            .filter(|event| event.action == PointerAction::Down)
            .collect();
        assert_eq!(downs.len(), 10, "{events:?}");
        let mut ids: Vec<u32> = downs.iter().map(|event| event.pointer_id).collect();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(
            ids.len(),
            10,
            "each finger keeps its own pointer: {events:?}"
        );
        assert!(count(&events, PointerAction::Move) >= 10, "{events:?}");
        assert!(events.iter().all(|event| event.task_id == 7));
        // Each finger lands on the task pixel under it.
        for down in &downs {
            let nearest = (0..10)
                .map(|index| at(index, 0))
                .map(|(x, y)| (down.x - x as f32).abs() + (down.y - y as f32).abs())
                .fold(f32::MAX, f32::min);
            assert!(
                nearest <= 2.0,
                "a finger landed away from every contact: {down:?}"
            );
        }
        // A finger is touch from start to end: Windows made no mouse input from it.
        let mouse = running
            .android
            .pointers
            .lock()
            .unwrap()
            .iter()
            .filter(|event| {
                event.tool == PointerTool::Mouse
                    && matches!(event.action, PointerAction::Down | PointerAction::Up)
            })
            .count();
        assert_eq!(mouse, 0, "a finger reached the task as a mouse");
    }

    #[test]
    #[ignore = "moves the machine's real pen input; the Installer test workflow runs it"]
    fn fr5_a_real_pen_on_a_task_window_reaches_its_task_with_its_pressure_barrel_and_eraser() {
        let running = start(None);
        running.android.start();
        running
            .android
            .publish(TaskEvent::Updated(task(7, "Paint", 800, 600)));
        let open = windows_of(&running, 1);
        let (width, height) = client_size(open[0].hwnd);
        let point = (width / 2, height * 3 / 4);
        let (_, origin) = window_on_top(&running, &[point]);
        let at = on_screen(origin, point);

        // As a real pen's digitizer reports it: the pen comes into range and hovers for a few
        // frames before it touches, and its other end is the eraser, which the digitizer
        // reports as inverted while it hovers and as the eraser while it touches. Turning
        // the pen over takes it out of range in between.
        let stylus = Device::new(PT_PEN, 1);
        for step in (1..=3).rev() {
            stylus.inject(&[pen(
                HOVERING,
                PEN_FLAG_BARREL,
                0,
                on_screen(origin, (point.0 - 4 * step, point.1)),
            )]);
        }
        stylus.inject(&[pen(HOVERING, PEN_FLAG_BARREL, 0, at)]);
        stylus.inject(&[pen(DOWN, PEN_FLAG_BARREL, 512, at)]);
        stylus.inject(&[pen(
            MOVING,
            PEN_FLAG_BARREL,
            768,
            on_screen(origin, (point.0 + 20, point.1)),
        )]);
        stylus.inject(&[pen(
            PEN_LIFTED,
            PEN_FLAG_BARREL,
            0,
            on_screen(origin, (point.0 + 20, point.1)),
        )]);
        stylus.inject(&[pen(
            OUT_OF_RANGE,
            0,
            0,
            on_screen(origin, (point.0 + 20, point.1)),
        )]);
        for step in (1..=3).rev() {
            stylus.inject(&[pen(
                HOVERING,
                PEN_FLAG_INVERTED,
                0,
                on_screen(origin, (point.0 - 4 * step, point.1)),
            )]);
        }
        stylus.inject(&[pen(HOVERING, PEN_FLAG_INVERTED, 0, at)]);
        stylus.inject(&[pen(DOWN, PEN_FLAG_INVERTED | PEN_FLAG_ERASER, 512, at)]);
        stylus.inject(&[pen(PEN_LIFTED, PEN_FLAG_INVERTED, 0, at)]);
        stylus.inject(&[pen(OUT_OF_RANGE, 0, 0, at)]);

        let tip = given(&running, PointerTool::Pen, "the pen to lift", |events| {
            count(events, PointerAction::Up) >= 1
        });
        let down = tip
            .iter()
            .find(|event| event.action == PointerAction::Down)
            .copied()
            .unwrap_or_else(|| panic!("the pen never pressed: {tip:?}"));
        assert!((down.pressure - 0.5).abs() <= 0.05, "{down:?}");
        assert_ne!(down.buttons & buttons::STYLUS_PRIMARY, 0, "{down:?}");
        assert_eq!((down.tilt_x, down.tilt_y), (30.0, -20.0), "{down:?}");
        assert!((down.x - point.0 as f32).abs() <= 2.0, "{down:?}");
        assert!((down.y - point.1 as f32).abs() <= 2.0, "{down:?}");
        assert!(count(&tip, PointerAction::HoverMove) >= 1, "{tip:?}");
        let eraser = given(
            &running,
            PointerTool::Eraser,
            "the eraser to lift",
            |events| count(events, PointerAction::Up) >= 1,
        );
        assert_eq!(count(&eraser, PointerAction::Down), 1, "{eraser:?}");
        // The pen goes out of range at the end, and the task hears it go. While Windows
        // holds a pen in range it can ignore touch ("Ignore touch input when I'm using my
        // pen"), so a pen left in range would keep every finger after it from every window.
        let deadline = Instant::now() + WAIT;
        loop {
            let all: Vec<PointerEvent> = running.android.pointers.lock().unwrap().clone();
            let pen_like =
                |event: &PointerEvent| matches!(event.tool, PointerTool::Pen | PointerTool::Eraser);
            let left = all
                .iter()
                .rposition(|event| {
                    event.tool == PointerTool::Eraser && event.action == PointerAction::Up
                })
                .is_some_and(|up| {
                    all[up + 1..]
                        .iter()
                        .any(|event| pen_like(event) && event.action == PointerAction::HoverExit)
                });
            if left {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for the pen to leave range; the task was given: {all:?}"
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    #[ignore = "moves the machine's real touch input; the Installer test workflow runs it"]
    fn fr5_a_real_finger_on_the_minimize_button_minimizes_the_task_window() {
        let running = start(None);
        running.android.start();
        running
            .android
            .publish(TaskEvent::Updated(task(7, "Paint", 800, 600)));
        let open = windows_of(&running, 1);
        let hwnd = open[0].hwnd as HWND;
        let (width, _) = client_size(open[0].hwnd);
        let mut origin = POINT { x: 0, y: 0 };
        unsafe { ClientToScreen(hwnd, &mut origin) };
        // The minimize button, wherever the title bar puts it.
        let button = (0..width)
            .step_by(2)
            .flat_map(|x| (0..64).step_by(2).map(move |y| (x, y)))
            .find(|&(x, y)| {
                let at = on_screen(origin, (x, y));
                let code = unsafe { SendMessageW(hwnd, WM_NCHITTEST, 0, lparam(at.x, at.y)) };
                code == HTMINBUTTON as isize
            })
            .expect("the title bar has a minimize button");
        let (_, origin) = window_on_top(&running, &[button]);
        let at = on_screen(origin, button);

        let fingers = Device::new(PT_TOUCH, 1);
        fingers.inject(&[finger(0, DOWN, at)]);
        fingers.inject(&[finger(0, LIFTED, at)]);

        wait_for(
            "a tap on the minimize button to minimize the window",
            || (unsafe { IsIconic(hwnd) } != 0).then_some(()),
        );
        assert!(
            running
                .android
                .pointers
                .lock()
                .unwrap()
                .iter()
                .all(|event| event.action != PointerAction::Down),
            "a tap on the title bar reached the task"
        );
    }

    /// Windows' touch gesture setting, put back to what it was when dropped.
    struct GesturesAsFound(u32);

    impl Drop for GesturesAsFound {
        fn drop(&mut self) {
            if let Err(why) = SystemGestures::default().write(self.0) {
                eprintln!(
                    "the touch gestures could not be put back to {}: {why}",
                    self.0
                );
            }
        }
    }

    fn wait_for_gestures(what: &str, wanted: impl Fn(u32) -> bool) -> u32 {
        let deadline = Instant::now() + WAIT;
        loop {
            // This thread's own window gets its messages.
            pump_messages();
            let now = SystemGestures::default()
                .read()
                .expect("reading the touch gestures (SystemParametersInfo 0x2030)");
            if wanted(now) {
                return now;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}: SystemParametersInfo 0x2030 reads {now}; the \
                 foreground window is {}",
                describe_window(unsafe { GetForegroundWindow() })
            );
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn pump_messages() {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            DispatchMessageW, PeekMessageW, TranslateMessage, MSG, PM_REMOVE,
        };
        // SAFETY: reads and dispatches this thread's own messages.
        unsafe {
            let mut message: MSG = std::mem::zeroed();
            while PeekMessageW(&mut message, std::ptr::null_mut(), 0, 0, PM_REMOVE) != 0 {
                TranslateMessage(&message);
                DispatchMessageW(&message);
            }
        }
    }

    /// A plain window of this test, the other program a user switches to.
    fn other_window() -> HWND {
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            CreateWindowExW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
        };
        let class: Vec<u16> = "STATIC\0".encode_utf16().collect();
        let title: Vec<u16> = "Another program\0".encode_utf16().collect();
        // SAFETY: a window of a system class, owned by this thread, with null-terminated
        // names that outlive the call.
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW | WS_VISIBLE,
                40,
                40,
                400,
                300,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                std::ptr::null(),
            )
        };
        assert!(!hwnd.is_null(), "{}", std::io::Error::last_os_error());
        hwnd
    }

    #[test]
    #[ignore = "changes the machine's real touch gestures; the Installer test workflow runs it"]
    fn fr5_real_touch_gestures_are_off_only_while_a_task_window_is_in_front() {
        use dsa_host_core::host_api::HostApiClient;
        use dsa_host_core::settings::Settings;
        use windows_sys::Win32::UI::WindowsAndMessaging::DestroyWindow;

        let found = SystemGestures::default()
            .read()
            .expect("reading the touch gestures (SystemParametersInfo 0x2030)");
        let _put_back = GesturesAsFound(found);
        // On, as Windows 11 has them unless the user turned them off.
        SystemGestures::default()
            .write(1)
            .expect("turning the touch gestures on (SystemParametersInfo 0x2031)");
        let original = 1;
        eprintln!("touch gestures: Windows read {found}; turned on ({original}) for the test");

        let record = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join(format!(
            "touch-gestures-original-{}.txt",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&record);
        let running = start_with(None, Some(TouchGestureSetup::system(record.clone())));
        // The setting, turned on through the local API as the settings app does.
        let mut client =
            HostApiClient::connect(&mut AsUser(running._api.clone()), "test settings", WAIT)
                .expect("connecting to the service");
        let on = client
            .set_settings(&Settings {
                touch_gestures_off: true,
                ..Settings::default()
            })
            .expect("turning the setting on");
        assert!(on.settings.touch_gestures_off);

        running.android.start();
        running
            .android
            .publish(TaskEvent::Updated(task(7, "Paint", 800, 600)));
        let open = windows_of(&running, 1);
        let (width, height) = client_size(open[0].hwnd);
        // The task window really is the foreground window before anything is asserted: on
        // the runners the shell's search pane can keep the foreground.
        let (task_window, _) = window_in_front(&running, &[(width / 2, height * 3 / 4)]);
        wait_for_gestures(
            "the gestures to go off with the task window in front",
            |now| now == 0,
        );
        assert_eq!(
            std::fs::read_to_string(&record).ok().as_deref(),
            Some("1"),
            "the user's value is kept while they are off"
        );
        eprintln!(
            "touch gestures: off with {} in front",
            describe_window(task_window)
        );

        let other = other_window();
        let deadline = Instant::now() + WAIT;
        while unsafe { GetForegroundWindow() } != other {
            assert!(
                Instant::now() < deadline,
                "timed out bringing another window to the front; the foreground window is {}",
                describe_window(unsafe { GetForegroundWindow() })
            );
            // An Alt tap lets this process move the foreground.
            unsafe {
                keybd_event(VK_MENU as u8, 0, 0, 0);
                keybd_event(VK_MENU as u8, 0, KEYEVENTF_KEYUP, 0);
                SetForegroundWindow(other);
            }
            pump_messages();
            thread::sleep(Duration::from_millis(50));
        }
        let back = wait_for_gestures(
            "the user's value to be back with another window in front",
            |now| now == original,
        );
        eprintln!("touch gestures: back to {back} with another window in front");
        // The window manager puts the value back first and removes the record after it, so
        // a crash in between leaves a record that the next start puts back again. The
        // record may still be there for a moment after the value reads back.
        let deadline = Instant::now() + WAIT;
        while record.exists() {
            assert!(
                Instant::now() < deadline,
                "nothing is kept once the value is back, yet {} still exists",
                record.display()
            );
            thread::sleep(Duration::from_millis(20));
        }

        // In front again, then the window manager stops: the value is back.
        unsafe { DestroyWindow(other) };
        window_in_front(&running, &[(width / 2, height * 3 / 4)]);
        wait_for_gestures("the gestures to go off again", |now| now == 0);
        drop(client);
        drop(running);
        wait_for_gestures(
            "the user's value to be back once the window manager stopped",
            |now| now == original,
        );
    }
}
