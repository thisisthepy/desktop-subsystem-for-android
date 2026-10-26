//! The Start menu shortcuts of the apps installed in the user's Android: real shortcut files
//! made by the shell in a folder of this test's own, read back the way the Start menu and
//! Windows search read them, and the window manager that keeps them as Android's apps change.
#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::compositor::FakeRenderer;
use dsa_host_core::host_api_conformance::{FakeAndroids, RunningService};
use dsa_host_core::tasks::{AppInfo, TaskEvent};
use dsa_host_windows::app_window::launch::ensure_window_manager;
use dsa_host_windows::app_window::{
    this_process_app_user_model_id, use_app_user_model_id, PACKAGE_ARGUMENT,
};
use dsa_host_windows::compositor::{Compositor, CompositorConfig};
use dsa_host_windows::start_menu::{
    app_user_model_id, ComApartment, ShellLinks, Shortcut, Shortcuts, StartMenu, StartMenuSetup,
};
use dsa_transport::local::{self, MemoryLocalConnector, Peer};
use dsa_transport::{Connection, Connector, Listener, TransportError};

const TEMPLATE: &str = env!("CARGO_BIN_EXE_dsa-app-window");
const WAIT: Duration = Duration::from_secs(20);
const USER: &str = "test-user";

fn fresh_dir(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_TARGET_TMPDIR"))
        .join("start-menu")
        .join(format!("{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    dir
}

fn setup(name: &str) -> StartMenuSetup {
    let dir = fresh_dir(name);
    StartMenuSetup {
        template: PathBuf::from(TEMPLATE),
        apps_dir: dir.join("apps"),
        menu: StartMenu {
            dir: dir.join("Programs"),
            note: dir.join("start-menu.json"),
        },
    }
}

/// The 256-pixel PNG of the DSA icon, standing in for an app's launcher icon.
fn png() -> Vec<u8> {
    let icon = include_bytes!("../installer/setup-ui/assets/logo.ico");
    dsa_exe_resources::parse_ico(icon)
        .unwrap()
        .into_iter()
        .find(|image| image.width == 0)
        .unwrap()
        .data
}

fn app(package: &str, label: &str) -> AppInfo {
    AppInfo {
        package_name: package.into(),
        label: label.into(),
        icon_png: png(),
    }
}

fn shortcut(setup: &StartMenuSetup, file: &str) -> Shortcut {
    ShellLinks
        .read(&setup.menu.dir.join(file))
        .unwrap_or_else(|why| panic!("{file}: {why}"))
}

fn files(dir: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

#[test]
fn fr7_each_installed_app_gets_a_shortcut_that_opens_it_under_its_own_app_id() {
    let _com = ComApartment::enter();
    let setup = setup("each");
    let problems = setup.sync(
        &[
            app("com.example.clock", "Clock"),
            app("com.example.maps", "Maps"),
        ],
        &ShellLinks,
    );
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(
        files(&setup.menu.dir),
        vec!["Clock.lnk".to_string(), "Maps.lnk".to_string()]
    );

    let clock = shortcut(&setup, "Clock.lnk");
    let program = setup.apps_dir.join("com.example.clock").join("Clock.exe");
    assert!(program.is_file(), "the app's window program is made");
    assert_eq!(clock.target, program);
    assert_eq!(
        clock.arguments,
        format!("{PACKAGE_ARGUMENT} com.example.clock")
    );
    assert_eq!(clock.icon, (program.clone(), 0));
    assert_eq!(clock.app_user_model_id, "DSA.com.example.clock");
    assert_eq!(
        clock.app_user_model_id,
        app_user_model_id("com.example.clock")
    );
    assert_ne!(
        shortcut(&setup, "Maps.lnk").app_user_model_id,
        clock.app_user_model_id,
        "each app is grouped on its own taskbar button"
    );
}

#[test]
fn fr7_a_removed_app_loses_its_shortcut() {
    let _com = ComApartment::enter();
    let setup = setup("removed");
    setup.sync(
        &[
            app("com.example.clock", "Clock"),
            app("com.example.maps", "Maps"),
        ],
        &ShellLinks,
    );
    let problems = setup.sync(&[app("com.example.clock", "Clock")], &ShellLinks);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(files(&setup.menu.dir), vec!["Clock.lnk".to_string()]);

    setup.sync(&[], &ShellLinks);
    assert!(files(&setup.menu.dir).is_empty());
}

#[test]
fn fr7_a_renamed_app_has_its_shortcut_renamed() {
    let _com = ComApartment::enter();
    let setup = setup("renamed");
    setup.sync(&[app("com.example.clock", "Clock")], &ShellLinks);
    setup.sync(&[app("com.example.clock", "Alarm")], &ShellLinks);
    assert_eq!(files(&setup.menu.dir), vec!["Alarm.lnk".to_string()]);
    assert_eq!(
        shortcut(&setup, "Alarm.lnk").target,
        setup.apps_dir.join("com.example.clock").join("Alarm.exe")
    );
}

#[test]
fn fr7_a_shortcut_dsa_did_not_make_is_never_touched() {
    let _com = ComApartment::enter();
    let setup = setup("foreign");
    std::fs::create_dir_all(&setup.menu.dir).unwrap();
    let foreign = Shortcut {
        target: PathBuf::from(r"C:\Windows\System32\notepad.exe"),
        arguments: String::new(),
        icon: (PathBuf::from(r"C:\Windows\System32\notepad.exe"), 0),
        working_dir: PathBuf::from(r"C:\Windows\System32"),
        description: "Someone else's clock".into(),
        app_user_model_id: "Example.Clock".into(),
    };
    ShellLinks
        .write(&setup.menu.dir.join("Clock.lnk"), &foreign)
        .unwrap();

    setup.sync(&[app("com.example.clock", "Clock")], &ShellLinks);
    assert_eq!(
        files(&setup.menu.dir),
        vec![
            "Clock (com.example.clock).lnk".to_string(),
            "Clock.lnk".to_string()
        ]
    );
    assert_eq!(shortcut(&setup, "Clock.lnk"), foreign);

    setup.sync(&[], &ShellLinks);
    assert_eq!(files(&setup.menu.dir), vec!["Clock.lnk".to_string()]);
    assert_eq!(shortcut(&setup, "Clock.lnk"), foreign);
}

#[test]
fn fr7_the_app_window_program_takes_its_apps_own_id() {
    use_app_user_model_id("com.example.clock").unwrap();
    assert_eq!(
        this_process_app_user_model_id().as_deref(),
        Some("DSA.com.example.clock")
    );
}

#[test]
fn fr7_opening_an_app_starts_the_window_manager_only_when_it_is_not_running() {
    let mut starts = 0;
    ensure_window_manager(
        || true,
        || {
            starts += 1;
            Ok(())
        },
        WAIT,
        Duration::from_millis(1),
    )
    .unwrap();
    assert_eq!(starts, 0);

    let mut asked = 0;
    ensure_window_manager(
        || {
            asked += 1;
            asked > 3
        },
        || {
            starts += 1;
            Ok(())
        },
        WAIT,
        Duration::from_millis(1),
    )
    .unwrap();
    assert_eq!(starts, 1);

    let never = ensure_window_manager(
        || false,
        || Ok(()),
        Duration::from_millis(20),
        Duration::from_millis(1),
    );
    assert!(never.is_err());
}

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

fn wait_for(what: &str, mut check: impl FnMut() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !check() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        thread::sleep(Duration::from_millis(20));
    }
}

#[test]
fn fr7_the_window_manager_keeps_the_start_menu_in_step_with_androids_apps() {
    let (api_listener, api_connector) = local::pair();
    let androids = Arc::new(FakeAndroids::default());
    let _service = RunningService::start(Box::new(api_listener), androids.clone());
    let unbound = || -> Result<Box<dyn Listener>, TransportError> {
        Err(TransportError::Unsupported("no VM in this test".into()))
    };
    let setup = setup("window-manager");
    let menu_dir = setup.menu.dir.clone();
    let compositor = Compositor::with_custom_transport(
        CompositorConfig {
            status_poll_interval: Duration::from_millis(50),
            ..Default::default()
        },
        Arc::new(FakeRenderer::new()),
        Box::new(move || Box::new(AsUser(api_connector.clone())) as Box<dyn Connector>),
        Box::new(move |_| unbound()),
        Box::new(move |_| unbound()),
    )
    .with_window_hosts(
        Box::new(|| {
            let (listener, _) = dsa_transport::memory::pair();
            Ok(Box::new(listener) as Box<dyn Listener>)
        }),
        Box::new(NoHosts),
    )
    .with_start_menu(setup);
    let stop = Arc::new(AtomicBool::new(false));
    let flag = Arc::clone(&stop);
    let thread = thread::spawn(move || compositor.run(flag));

    wait_for("the window manager to watch the tasks", || {
        androids.of(USER).is_some()
    });
    let android = androids.of(USER).expect("the user's Android");
    android.publish(TaskEvent::Installed(vec![
        app("com.example.clock", "Clock"),
        app("com.example.maps", "Maps"),
    ]));
    wait_for("both shortcuts", || files(&menu_dir).len() == 2);

    android.publish(TaskEvent::Installed(vec![app(
        "com.example.clock",
        "Clock",
    )]));
    wait_for("the removed app's shortcut to go", || {
        files(&menu_dir) == vec!["Clock.lnk".to_string()]
    });

    stop.store(true, Ordering::SeqCst);
    let _ = thread.join();
}

/// Starts no window host: this test has no tasks.
struct NoHosts;

impl dsa_host_windows::compositor::hosts::HostLauncher for NoHosts {
    fn start(&mut self, _: &str, _: Option<&AppInfo>) -> Result<(), String> {
        Ok(())
    }
}
