//! The compositor's task windows, as native windows on this machine: one per guest task,
//! opened and closed with its task, and a window close that ends its task.
//!
//! The service runs on the in-memory local API with a fake Android whose tasks the tests
//! change, as the guest's task messages would.
#![cfg(windows)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::FakeRenderer;
use dsa_host_core::host_api::UserAndroid;
use dsa_host_core::host_api_conformance::{FakeAndroid, FakeAndroids, RunningService};
use dsa_host_core::tasks::{Bounds, Task, TaskEvent, TaskRequest};
use dsa_host_windows::compositor::adapter::{DisplayPresenter, DisplaySink};
use dsa_host_windows::compositor::window::OpenWindow;
use dsa_host_windows::compositor::{Compositor, CompositorConfig};
use dsa_transport::local::{self, MemoryLocalConnector, Peer};
use dsa_transport::{Connection, Connector, Listener, TransportError};
use windows_sys::Win32::Foundation::{HWND, RECT};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetClientRect, GetWindowTextW, IsWindow, IsWindowVisible, PostMessageW, SetWindowPos,
    SWP_NOMOVE, SWP_NOZORDER, WM_CLOSE,
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

/// A presenter that keeps the sink the compositor gives it, so a test can post frames.
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

struct Running {
    android: Arc<FakeAndroid>,
    windows: Arc<Mutex<Vec<OpenWindow>>>,
    stop: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
    _service: RunningService,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
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
    let mut compositor = Compositor::with_custom_transport(
        config,
        Arc::new(FakeRenderer::new()),
        Box::new(move || Box::new(AsUser(api_connector.clone())) as Box<dyn Connector>),
        Box::new(move |_| unbound()),
        Box::new(move |_| unbound()),
    );
    if let Some(presenter) = presenter {
        compositor = compositor.with_display_presenter(presenter);
    }
    let windows = compositor.open_windows();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = thread::spawn(move || compositor.run(flag));
    let android = wait_for("the compositor to watch the tasks", || androids.of(USER));
    Running {
        android,
        windows,
        stop,
        thread: Some(thread),
        _service: service,
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
        let open = running.windows.lock().unwrap().clone();
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

#[test]
fn fr3_two_apps_open_two_native_windows_titled_and_sized_by_their_tasks() {
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
    assert!(running.windows.lock().unwrap().is_empty());
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
        let now = running.windows.lock().unwrap().clone();
        (now.len() == 1 && now[0].hwnd != open[0].hwnd).then_some(now)
    });
    assert_eq!(title(reopened[0].hwnd), "Clock");
}

#[test]
fn fr3_only_a_tasks_own_display_reaches_its_window_and_the_whole_screen_never_does() {
    let presenter = Arc::new(CapturingPresenter::default());
    let running = start(Some(Arc::clone(&presenter)));
    let Some(sink) = presenter.sink.lock().unwrap().clone() else {
        // The compositor could not create its Direct3D devices here, so it showed no frames
        // and registered no sink. The windows tests above still ran.
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

    let handle = wait_for("display 3 to reach task 1's window", || {
        sink.target(3, 400, 300)
    });
    assert_ne!(handle, 0);
    // Copying the frame into the window's swapchain and presenting it works on this GPU.
    sink.frame(3);
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
    assert_eq!(sink.target(3, 640, 480), None);
}

#[test]
fn nfr5_a_service_that_goes_away_closes_every_task_window() {
    let (api_listener, api_connector) = local::pair();
    let androids = Arc::new(FakeAndroids::default());
    let service = RunningService::start(Box::new(api_listener), androids.clone());
    let unbound = || -> Result<Box<dyn Listener>, TransportError> {
        Err(TransportError::Unsupported("no VM in this test".into()))
    };
    let compositor = Compositor::with_custom_transport(
        CompositorConfig::default(),
        Arc::new(FakeRenderer::new()),
        Box::new(move || Box::new(AsUser(api_connector.clone())) as Box<dyn Connector>),
        Box::new(move |_| unbound()),
        Box::new(move |_| unbound()),
    );
    let windows = compositor.open_windows();
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = thread::spawn(move || compositor.run(flag));
    let android = wait_for("the compositor to watch the tasks", || androids.of(USER));
    android.publish(TaskEvent::Updated(task(1, "Clock", 400, 300)));
    wait_for("the window", || {
        (windows.lock().unwrap().len() == 1).then_some(())
    });

    // The service stops: no new connection is accepted, and the watch ends.
    drop(service);
    android.end_watches();
    wait_for("every window to close", || {
        windows.lock().unwrap().is_empty().then_some(())
    });
    stop.store(true, Ordering::SeqCst);
    thread.join().unwrap();
}
