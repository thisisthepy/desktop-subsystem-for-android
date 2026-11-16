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
use dsa_host_windows::app_window::launch::{ensure_window_manager, prepare_app};
use dsa_host_windows::app_window::{
    this_process_app_user_model_id, use_app_user_model_id, PACKAGE_ARGUMENT,
};
use dsa_host_windows::compositor::{Compositor, CompositorConfig};
use dsa_host_windows::desktop_shortcuts::Desktop;
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
        desktop: Some(Desktop {
            dir: dir.join("Desktop"),
            note: dir.join("desktop-shortcuts.json"),
        }),
        pins: vec![dir.join("TaskBar"), dir.join("ImplicitAppShortcuts")],
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
        ..Default::default()
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
    assert_eq!(clock.app_user_model_id, "Thisisthepy.DSA.com.example.clock");
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
    let problems = setup.sync(&[app("com.example.clock", "Clock")], &ShellLinks);
    assert!(problems.is_empty(), "{problems:?}");
    let folder = setup.apps_dir.join("com.example.clock");
    let clock = folder.join("Clock.exe");
    // The user pinned the app to the taskbar (Windows copies the Start menu shortcut) and put
    // it on the desktop.
    let pinned = shortcut(&setup, "Clock.lnk");
    let pin = setup.pins[0].join("Clock.lnk");
    let implicit = setup.pins[1].join("abc123").join("Clock.lnk");
    std::fs::create_dir_all(pin.parent().unwrap()).unwrap();
    std::fs::create_dir_all(implicit.parent().unwrap()).unwrap();
    ShellLinks.write(&pin, &pinned).unwrap();
    ShellLinks.write(&implicit, &pinned).unwrap();
    let desktop = setup.desktop.as_ref().unwrap();
    let entry = dsa_host_windows::start_menu::MenuEntry {
        package: "com.example.clock".into(),
        label: "Clock".into(),
        program: clock.clone(),
    };
    desktop.place_app(&entry, &ShellLinks).unwrap();
    assert_eq!(files(&desktop.dir), vec!["Clock.lnk".to_string()]);

    let problems = setup.sync(&[app("com.example.clock", "Alarm")], &ShellLinks);
    assert!(problems.is_empty(), "{problems:?}");
    assert_eq!(files(&setup.menu.dir), vec!["Alarm.lnk".to_string()]);
    // The program's file follows the app's name, and the one of the old name is gone.
    let program = folder.join("Alarm.exe");
    assert!(program.is_file());
    assert!(!clock.exists());
    assert_eq!(shortcut(&setup, "Alarm.lnk").target, program);
    assert_eq!(shortcut(&setup, "Alarm.lnk").icon, (program.clone(), 0));
    // Every shortcut followed it in the same step.
    for pin in [&pin, &implicit] {
        let now = ShellLinks.read(pin).unwrap();
        assert_eq!(now.target, program, "{}", pin.display());
        assert_eq!(now.icon, (program.clone(), 0));
        assert_eq!(now.app_user_model_id, pinned.app_user_model_id);
        assert_eq!(now.arguments, pinned.arguments);
    }
    assert_eq!(files(&desktop.dir), vec!["Alarm.lnk".to_string()]);
    assert_eq!(
        ShellLinks
            .read(&desktop.dir.join("Alarm.lnk"))
            .unwrap()
            .target,
        program
    );
}

#[test]
fn fr7_a_shortcut_whose_program_is_gone_is_pointed_at_the_apps_program_again() {
    // An earlier release left the Start menu shortcut, the desktop shortcuts and the taskbar
    // pin pointing at a program of an older name, and a program of another name beside it.
    use dsa_host_windows::start_menu::{menu_entries, repair_pins};
    let _com = ComApartment::enter();
    let setup = setup("repair");
    let package = "com.android.vending";
    let folder = setup.apps_dir.join(package);
    std::fs::create_dir_all(&folder).unwrap();
    let other = folder.join("Google Play Store.exe");
    std::fs::copy(TEMPLATE, &other).unwrap();
    let gone = folder.join("Store.exe");
    let stale = Shortcut {
        target: gone.clone(),
        arguments: format!("{PACKAGE_ARGUMENT} {package}"),
        icon: (gone.clone(), 0),
        working_dir: folder.clone(),
        description: "Play Store (Desktop Subsystem for Android)".into(),
        app_user_model_id: app_user_model_id(package),
    };
    std::fs::create_dir_all(&setup.menu.dir).unwrap();
    ShellLinks
        .write(&setup.menu.dir.join("Play Store.lnk"), &stale)
        .unwrap();
    std::fs::write(
        &setup.menu.note,
        format!(r#"{{"shortcuts":{{"{package}":"Play Store.lnk"}}}}"#),
    )
    .unwrap();
    let root = setup.menu.note.parent().unwrap().to_path_buf();
    let pins = root.join("TaskBar");
    std::fs::create_dir_all(&pins).unwrap();
    ShellLinks
        .write(&pins.join("Play Store.lnk"), &stale)
        .unwrap();
    // Someone else's pin is never touched, even when its program is gone too.
    let foreign = Shortcut {
        target: root.join("missing.exe"),
        arguments: String::new(),
        icon: (root.join("missing.exe"), 0),
        working_dir: root.clone(),
        description: String::new(),
        app_user_model_id: "Example.Other".into(),
    };
    ShellLinks.write(&pins.join("Other.lnk"), &foreign).unwrap();
    let desktop = setup.desktop.as_ref().unwrap();
    std::fs::create_dir_all(&desktop.dir).unwrap();
    let on_desktop = desktop.dir.join("Play Store.lnk");
    ShellLinks.write(&on_desktop, &stale).unwrap();
    let app_shortcut = desktop.dir.join("Search.lnk");
    let search = Shortcut {
        arguments: format!("{PACKAGE_ARGUMENT} {package} --shortcut search"),
        icon: (folder.join("shortcuts").join("search.ico"), 0),
        ..stale.clone()
    };
    ShellLinks.write(&app_shortcut, &search).unwrap();
    std::fs::write(
        &desktop.note,
        serde_json::json!({ "apps": { package: {
            "app": on_desktop,
            "shortcuts": { "search": app_shortcut },
        } } })
        .to_string(),
    )
    .unwrap();

    // DSA starts and Android lists the app: its program takes the listed name, and every
    // shortcut to the app follows it before the program of the other name goes.
    let apps = [app(package, "Play Store")];
    let problems = setup.sync(&apps, &ShellLinks);
    assert!(problems.is_empty(), "{problems:?}");
    let program = folder.join("Play Store.exe");
    assert!(program.is_file(), "the program is named after the app");
    assert!(!other.exists());
    assert!(!gone.exists());
    assert_eq!(shortcut(&setup, "Play Store.lnk").target, program);

    let pin = ShellLinks.read(&pins.join("Play Store.lnk")).unwrap();
    assert_eq!(pin.target, program);
    assert_eq!(pin.icon, (program.clone(), 0));
    assert_eq!(pin.working_dir, folder);
    assert_eq!(pin.arguments, stale.arguments);
    assert_eq!(pin.app_user_model_id, app_user_model_id(package));
    assert_eq!(ShellLinks.read(&pins.join("Other.lnk")).unwrap(), foreign);

    assert_eq!(ShellLinks.read(&on_desktop).unwrap().target, program);
    let search_now = ShellLinks.read(&app_shortcut).unwrap();
    assert_eq!(search_now.target, program);
    assert_eq!(search_now.arguments, search.arguments);
    assert_eq!(search_now.icon, search.icon, "its own icon stays");

    // A pin left pointing at a program that is gone, with no other program beside the app's,
    // is pointed at it when the list of installed apps comes.
    let late = pins.join("Late.lnk");
    ShellLinks.write(&late, &stale).unwrap();
    let entries = menu_entries(&setup.apps_dir, &apps);
    assert!(repair_pins(std::slice::from_ref(&pins), &entries, &ShellLinks).is_empty());
    assert_eq!(ShellLinks.read(&late).unwrap().target, program);
    ShellLinks.write(&app_shortcut, &search).unwrap();
    assert!(desktop.keep_in_step(&entries, &ShellLinks).is_empty());
    assert_eq!(ShellLinks.read(&app_shortcut).unwrap().target, program);
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
        Some("Thisisthepy.DSA.com.example.clock")
    );
}

#[test]
fn fr7_opening_an_app_starts_the_window_manager_before_it_waits_for_android() {
    let order = std::cell::RefCell::new(Vec::new());
    prepare_app(
        || {
            order.borrow_mut().push("window manager");
            Ok(())
        },
        || {
            order.borrow_mut().push("android");
            Ok(())
        },
    )
    .unwrap();
    assert_eq!(*order.borrow(), ["window manager", "android"]);

    let failed = prepare_app(
        || Err("not installed".to_string()),
        || panic!("Android would wait for a renderer that is not coming"),
    );
    assert!(failed.is_err());
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
