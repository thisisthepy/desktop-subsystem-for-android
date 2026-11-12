//! The window manager's rules: which task has a window, which app's window host shows it,
//! and where each display's frames go.
//!
//! One thread, the manager, keeps the guest's tasks (`TaskWindows`) and the window hosts
//! (`WindowHosts`) and carries out what they decide through a [`Platform`]: it starts each
//! app's window program, sends each window host its windows, and asks the guest to end a task
//! whose window the user closed. Nothing here calls a macOS framework, so the rules are tested
//! on every build host.
//!
//! A minimized window gets no frames: its route is withdrawn, so the frame surfaces of its
//! task are released until the window is shown again, and once no window is shown the guest
//! is told so and may sleep.

use std::collections::BTreeSet;
use std::time::Instant;

use dsa_host_core::app_windows::AppWindowSettings;
use dsa_host_core::compositor::shown_windows::ShownWindows;
use dsa_host_core::compositor::task_windows::{TaskWindows, WindowChange};
use dsa_host_core::compositor::window_hosts::{
    is_valid_package, FromHost, HostAction, HostId, MenuWork, ToHost, WindowHosts,
};
use dsa_host_core::tasks::{AppInfo, PinShortcutRequest, TaskEvent, TaskRequest};

/// The URL scheme DSA's settings app (DSA.app) opens, which a running copy receives too.
pub const SETTINGS_SCHEME: &str = "dsa-settings";

/// The link that opens DSA's settings on the Apps page with `package`'s row in view, as a
/// task window's App Info does: `dsa-settings://apps/com.example.notes`. `None` for a name
/// that is no package, which never reaches the settings app.
pub fn app_page_url(package: &str) -> Option<String> {
    is_valid_package(package).then(|| format!("{SETTINGS_SCHEME}://apps/{package}"))
}

/// What the other threads tell the manager.
pub enum ManagerEvent {
    /// A new watch began; these are the guest's tasks now.
    Sync(Vec<TaskEvent>),
    /// A change to the guest's tasks. Boxed, as an app's event carries its icons.
    Task(Box<TaskEvent>),
    /// The service cannot be reached, so the guest's tasks are unknown.
    Lost,
    /// The request to end a task whose window the user closed did not reach the guest.
    CloseFailed(i32),
    /// A window host connected and said which app it shows.
    Attached {
        host: HostId,
        package: String,
        launch: bool,
        /// With `launch`, the shortcut of the app to start rather than the app; empty for none.
        shortcut: String,
    },
    /// A window host reported what the user did to a window.
    Report { host: HostId, report: FromHost },
    /// A window host's connection ended.
    Gone { host: HostId },
    /// The user's window settings for each app, as the service has them now.
    AppWindows(Vec<AppWindowSettings>),
}

/// Where one display's frames go: the task and the window host that shows its window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Route {
    pub display_id: u32,
    pub task_id: i32,
    pub host: HostId,
}

/// What the manager needs done outside itself.
pub trait Platform {
    /// Starts the window host of `package`, named and drawn as `app` when it is known. The
    /// window host then connects by itself.
    fn start_host(&mut self, package: &str, app: Option<&AppInfo>) -> Result<(), String>;
    /// Sends `message` to the window host on `host`.
    fn send(&mut self, host: HostId, message: &ToHost);
    /// Ends the connection `host`; its window host then exits.
    fn disconnect(&mut self, host: HostId);
    /// Sends `request` to the guest.
    fn request(&mut self, request: TaskRequest);
    /// From now on the frames of each display go as `routes` say, and no others are shown.
    fn routes(&mut self, routes: &[Route]);
    /// The apps installed in the guest changed: the app bundles follow them.
    fn installed(&mut self, _apps: &[AppInfo]) {}
    /// Whether a task window is the active window now.
    fn focused(&mut self, _any: bool) {}
    /// An app asks for one of its shortcuts on the Desktop: the user is asked, and the
    /// answer goes to Android. Without a way to ask, the app hears no.
    fn pin_requested(&mut self, request: &PinShortcutRequest) {
        self.request(TaskRequest::AnswerPinShortcut {
            request_id: request.request_id,
            accepted: false,
        });
    }
    /// Opens DSA's settings on the Apps page at `package`, as a window's App Info does.
    fn open_app_page(&mut self, package: &str) -> Result<(), String>;
    /// Writes a line to the log.
    fn log(&mut self, line: &str);
}

/// The window manager's state.
pub struct Manager<P: Platform> {
    windows: TaskWindows,
    hosts: WindowHosts,
    shown: ShownWindows,
    /// The task windows that are the active window.
    focused: BTreeSet<i32>,
    /// Whether a task window was the active window when the platform was last told.
    told_focused: bool,
    /// The task windows that are minimized, which get no frames.
    hidden: BTreeSet<i32>,
    /// The routes the platform was last given.
    routes: Option<Vec<Route>>,
    platform: P,
}

impl<P: Platform> Manager<P> {
    pub fn new(platform: P) -> Manager<P> {
        Manager {
            windows: TaskWindows::new(),
            hosts: WindowHosts::new(),
            shown: ShownWindows::new(),
            focused: BTreeSet::new(),
            told_focused: false,
            hidden: BTreeSet::new(),
            routes: None,
            platform,
        }
    }

    pub fn platform(&self) -> &P {
        &self.platform
    }

    pub fn platform_mut(&mut self) -> &mut P {
        &mut self.platform
    }

    /// The tasks that have a window, in task ID order.
    pub fn windows(&self) -> Vec<i32> {
        self.windows.windows()
    }

    /// Takes one event, then tells the platform what changed.
    pub fn handle(&mut self, event: ManagerEvent, now: Instant) {
        match event {
            ManagerEvent::Sync(current) => {
                // A new watch may follow a restart of the service, which forgot the word on
                // shown windows.
                self.shown.retell();
                for event in &current {
                    self.app_event(event, now);
                }
                let changes = self.windows.sync(&current);
                self.change(changes, now);
            }
            ManagerEvent::Task(event) => {
                self.app_event(&event, now);
                let changes = self.windows.apply(&event);
                for note in self.windows.take_notes() {
                    self.platform.log(&note);
                }
                self.change(changes, now);
                // A task that took a size its window gave it live outside a drag is
                // reconfigured for it now, at a size it has.
                let requests = self.windows.reconfigures(now);
                self.request(requests);
            }
            ManagerEvent::Lost => {
                let changes = self.windows.reset();
                self.change(changes, now);
            }
            ManagerEvent::CloseFailed(task_id) => {
                self.platform.log(&format!(
                    "the request to end task {task_id} did not reach Android; its window opens \
                     again"
                ));
                let changes = self.windows.close_failed(task_id);
                self.change(changes, now);
            }
            ManagerEvent::Attached {
                host,
                package,
                launch,
                shortcut,
            } => {
                let actions = self.hosts.attached_for(
                    host,
                    &package,
                    launch,
                    Some(shortcut.as_str()).filter(|id| !id.is_empty()),
                    now,
                );
                self.carry_out(actions);
            }
            ManagerEvent::Report { host, report } => {
                // A window host may only speak of its own windows.
                if let Some(report) = self.hosts.from_host(host, report) {
                    self.reported(report, now);
                }
            }
            ManagerEvent::Gone { host } => {
                for task_id in self.hosts.detached(host) {
                    // Ending an app's window program closes its windows, as ending any
                    // program does: Android ends their tasks.
                    self.user_closed(task_id, now);
                }
            }
            ManagerEvent::AppWindows(apps) => {
                let changes = self.windows.set_app_windows(&apps);
                self.change(changes, now);
            }
        }
        self.publish();
    }

    /// Looks at the timers: windows whose task did not take their size, and window hosts
    /// that are late.
    pub fn tick(&mut self, now: Instant) {
        let changes = self.windows.tick(now);
        self.change(changes, now);
        let requests = self.windows.reconfigures(now);
        self.request(requests);
        let (failed, actions) = self.hosts.tick(now);
        for (package, tasks) in failed {
            self.platform.log(&format!(
                "{package}'s window program did not start in time; {} window(s) stay closed",
                tasks.len()
            ));
        }
        self.carry_out(actions);
        self.publish();
    }

    /// The window manager stops: every window host is let go and no frame is routed.
    pub fn finish(&mut self) {
        let actions = self.hosts.reset();
        self.carry_out(actions);
        self.routes = None;
        self.platform.routes(&[]);
    }

    fn app_event(&mut self, event: &TaskEvent, now: Instant) {
        match event {
            TaskEvent::App(app) => {
                let actions = self.hosts.app(app, now);
                self.carry_out(actions);
            }
            TaskEvent::Installed(apps) => self.platform.installed(apps),
            TaskEvent::PinShortcut(request) => self.platform.pin_requested(request),
            _ => {}
        }
    }

    fn reported(&mut self, report: FromHost, now: Instant) {
        match report {
            FromHost::Closed { task_id } => self.user_closed(task_id, now),
            // The task takes its window's client size, and draws at its display's density.
            FromHost::Resized {
                task_id,
                width,
                height,
            } => {
                let requests = self.windows.window_resized(task_id, (width, height), now);
                self.request(requests);
            }
            FromHost::Dpi { task_id, dpi } => {
                let (changes, requests) = self.windows.window_dpi(task_id, dpi);
                self.request(requests);
                self.change(changes, now);
            }
            FromHost::Dragging { task_id, dragging } => {
                let requests = self.windows.window_dragging(task_id, dragging, now);
                self.request(requests);
            }
            FromHost::Caption { task_id, band } => {
                let requests = self.windows.window_caption(task_id, band);
                self.request(requests);
            }
            FromHost::Focus { task_id, focused } => {
                if focused {
                    self.focused.insert(task_id);
                } else {
                    self.focused.remove(&task_id);
                }
            }
            FromHost::Shown { task_id, shown } => {
                self.shown.reported(task_id, shown);
                if shown {
                    self.hidden.remove(&task_id);
                } else {
                    self.hidden.insert(task_id);
                }
            }
            // An Android entry of a window's Window menu: Android opens its page, or DSA's
            // settings open the app's.
            FromHost::MenuChosen { task_id, action } => {
                match self.hosts.menu_chosen(task_id, action) {
                    Some(MenuWork::Request(request)) => self.platform.request(request),
                    Some(MenuWork::AppPage { package }) => {
                        if let Err(why) = self.platform.open_app_page(&package) {
                            self.platform
                                .log(&format!("could not open the settings of {package}: {why}"));
                        }
                    }
                    None => {}
                }
            }
            // Only a Windows taskbar preview has media buttons; these windows send none.
            FromHost::MediaButton { .. } => {}
        }
    }

    fn request(&mut self, requests: Vec<TaskRequest>) {
        for request in requests {
            self.platform.request(request);
        }
    }

    fn user_closed(&mut self, task_id: i32, now: Instant) {
        let Some(request) = self.windows.user_closed(task_id) else {
            return;
        };
        self.platform.request(request);
        self.change(vec![WindowChange::Close { task_id }], now);
    }

    fn change(&mut self, changes: Vec<WindowChange>, now: Instant) {
        for change in changes {
            let actions = self.hosts.change(&change, now);
            self.carry_out(actions);
        }
    }

    fn carry_out(&mut self, actions: Vec<HostAction>) {
        for action in actions {
            match action {
                HostAction::Start { package, app } => {
                    if let Err(why) = self.platform.start_host(&package, app.as_deref()) {
                        let line = format!("could not start {package}'s window program: {why}");
                        self.platform.log(&line);
                        self.hosts.start_failed(&package);
                    }
                }
                HostAction::Send { host, message } => {
                    self.platform.send(host, &message);
                }
                HostAction::Launch { package, shortcut } => {
                    self.platform.request(match shortcut {
                        Some(shortcut_id) => TaskRequest::LaunchShortcut {
                            package_name: package,
                            shortcut_id,
                        },
                        None => TaskRequest::Launch {
                            package_name: package,
                        },
                    });
                }
                HostAction::Disconnect { host, why } => {
                    self.platform
                        .log(&format!("closing an app window program: {why}"));
                    self.platform.disconnect(host);
                }
            }
        }
    }

    /// Tells the guest when whether any window is shown changed, and the platform where each
    /// display's frames go when that changed.
    fn publish(&mut self) {
        let windows = self.windows.windows();
        self.hidden.retain(|task_id| windows.contains(task_id));
        self.focused.retain(|task_id| windows.contains(task_id));
        let focused = !self.focused.is_empty();
        if focused != self.told_focused {
            self.told_focused = focused;
            self.platform.focused(focused);
        }
        if let Some(request) = self.shown.update(&windows) {
            self.platform.request(request);
        }
        let routes: Vec<Route> = self
            .windows
            .routes()
            .into_iter()
            .filter(|(_, task_id)| !self.hidden.contains(task_id))
            .filter_map(|(display_id, task_id)| {
                Some(Route {
                    display_id,
                    task_id,
                    host: self.hosts.host_of(task_id)?,
                })
            })
            .collect();
        if self.routes.as_ref() != Some(&routes) {
            self.platform.routes(&routes);
            self.routes = Some(routes);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dsa_host_core::tasks::{Bounds, Task};

    #[derive(Default)]
    struct Fake {
        started: Vec<String>,
        sent: Vec<(HostId, ToHost)>,
        requests: Vec<TaskRequest>,
        routes: Vec<Route>,
        installed: Vec<String>,
        focused: bool,
        refuse_start: bool,
        app_pages: Vec<String>,
    }

    impl Platform for Fake {
        fn start_host(&mut self, package: &str, _app: Option<&AppInfo>) -> Result<(), String> {
            if self.refuse_start {
                return Err("no bundle".into());
            }
            self.started.push(package.to_string());
            Ok(())
        }
        fn send(&mut self, host: HostId, message: &ToHost) {
            self.sent.push((host, message.clone()));
        }
        fn disconnect(&mut self, _host: HostId) {}
        fn request(&mut self, request: TaskRequest) {
            self.requests.push(request);
        }
        fn routes(&mut self, routes: &[Route]) {
            self.routes = routes.to_vec();
        }
        fn installed(&mut self, apps: &[AppInfo]) {
            self.installed = apps.iter().map(|a| a.package_name.clone()).collect();
        }
        fn focused(&mut self, any: bool) {
            self.focused = any;
        }
        fn open_app_page(&mut self, package: &str) -> Result<(), String> {
            self.app_pages.push(package.to_string());
            Ok(())
        }
        fn log(&mut self, _line: &str) {}
    }

    const PACKAGE: &str = "com.example.notes";
    const HOST: HostId = HostId(1);

    fn app() -> AppInfo {
        AppInfo {
            package_name: PACKAGE.into(),
            label: "Notes".into(),
            ..AppInfo::default()
        }
    }

    fn task(task_id: i32) -> Task {
        Task {
            task_id,
            package_name: PACKAGE.into(),
            label: "Notes".into(),
            bounds: Bounds {
                left: 0,
                top: 0,
                right: 400,
                bottom: 600,
            },
            density_dpi: 262,
            ..Task::default()
        }
    }

    /// A manager with task 7 open in an attached window host, its frames on display 3.
    fn with_window() -> Manager<Fake> {
        let now = Instant::now();
        let mut manager = Manager::new(Fake::default());
        manager.handle(ManagerEvent::Task(Box::new(TaskEvent::App(app()))), now);
        manager.handle(
            ManagerEvent::Task(Box::new(TaskEvent::Updated(task(7)))),
            now,
        );
        let attached = ManagerEvent::Attached {
            host: HOST,
            package: PACKAGE.into(),
            launch: false,
            shortcut: String::new(),
        };
        manager.handle(attached, now);
        let frames = TaskEvent::Frames {
            task_id: 7,
            display_id: 3,
        };
        manager.handle(ManagerEvent::Task(Box::new(frames)), now);
        manager
    }

    fn report_from(manager: &mut Manager<Fake>, host: HostId, report: FromHost) {
        manager.handle(ManagerEvent::Report { host, report }, Instant::now());
    }

    fn report(manager: &mut Manager<Fake>, report: FromHost) {
        report_from(manager, HOST, report);
    }

    fn asked(manager: &Manager<Fake>, request: &TaskRequest) -> bool {
        manager.platform().requests.contains(request)
    }

    #[test]
    fn fr3_a_task_opens_its_window_in_its_apps_own_window_host() {
        let manager = with_window();
        assert_eq!(manager.platform().started, vec![PACKAGE.to_string()]);
        let shown = manager.platform().sent.iter().any(|(host, message)| {
            *host == HOST && matches!(message, ToHost::Show { task_id: 7, .. })
        });
        assert!(shown);
        assert_eq!(manager.windows(), vec![7]);
    }

    #[test]
    fn fr3_the_tasks_frames_go_to_its_window_and_no_other_display_is_shown() {
        let manager = with_window();
        let route = Route {
            display_id: 3,
            task_id: 7,
            host: HOST,
        };
        assert_eq!(manager.platform().routes, vec![route]);
    }

    #[test]
    fn fr3_closing_the_window_asks_android_to_end_the_task() {
        let mut manager = with_window();
        report(&mut manager, FromHost::Closed { task_id: 7 });
        assert!(asked(&manager, &TaskRequest::Close { task_id: 7 }));
        assert!(manager.platform().routes.is_empty());
    }

    #[test]
    fn fr4_a_resized_window_resizes_its_task() {
        let mut manager = with_window();
        let resized = FromHost::Resized {
            task_id: 7,
            width: 800,
            height: 500,
        };
        report(&mut manager, resized);
        let resizes = manager.platform().requests.iter().any(|request| {
            matches!(
                request,
                TaskRequest::Resize {
                    task_id: 7,
                    width: 800,
                    height: 500,
                    ..
                }
            )
        });
        assert!(resizes);
    }

    #[test]
    fn fr10_a_minimized_window_releases_its_frames_and_lets_android_sleep() {
        let mut manager = with_window();
        let hidden = FromHost::Shown {
            task_id: 7,
            shown: false,
        };
        report(&mut manager, hidden);
        assert!(manager.platform().routes.is_empty());
        assert!(asked(&manager, &TaskRequest::WindowsShown { shown: false }));
        let shown = FromHost::Shown {
            task_id: 7,
            shown: true,
        };
        report(&mut manager, shown);
        assert_eq!(manager.platform().routes.len(), 1);
        let last = manager.platform().requests.last();
        assert_eq!(last, Some(&TaskRequest::WindowsShown { shown: true }));
    }

    #[test]
    fn fr3_a_window_host_that_exits_ends_its_tasks() {
        let mut manager = with_window();
        manager.handle(ManagerEvent::Gone { host: HOST }, Instant::now());
        assert!(asked(&manager, &TaskRequest::Close { task_id: 7 }));
        assert!(manager.platform().routes.is_empty());
    }

    #[test]
    fn nfr5_a_window_host_cannot_speak_of_another_apps_window() {
        let mut manager = with_window();
        report_from(&mut manager, HostId(99), FromHost::Closed { task_id: 7 });
        assert!(!asked(&manager, &TaskRequest::Close { task_id: 7 }));
        assert_eq!(manager.windows(), vec![7]);
    }

    #[test]
    fn fr3_a_window_program_that_cannot_start_is_reported_and_left() {
        let now = Instant::now();
        let fake = Fake {
            refuse_start: true,
            ..Fake::default()
        };
        let mut manager = Manager::new(fake);
        manager.handle(ManagerEvent::Task(Box::new(TaskEvent::App(app()))), now);
        manager.handle(
            ManagerEvent::Task(Box::new(TaskEvent::Updated(task(7)))),
            now,
        );
        assert!(manager.platform().started.is_empty());
        assert!(manager.platform().routes.is_empty());
    }

    #[test]
    fn fr7_the_installed_apps_reach_the_app_bundles() {
        let mut manager = Manager::new(Fake::default());
        let installed = TaskEvent::Installed(vec![app()]);
        manager.handle(ManagerEvent::Task(Box::new(installed)), Instant::now());
        assert_eq!(manager.platform().installed, vec![PACKAGE.to_string()]);
    }

    #[test]
    fn fr30_focus_follows_the_active_task_window() {
        let mut manager = with_window();
        let focused = FromHost::Focus {
            task_id: 7,
            focused: true,
        };
        report(&mut manager, focused);
        assert!(manager.platform().focused);
        manager.handle(ManagerEvent::Gone { host: HOST }, Instant::now());
        assert!(!manager.platform().focused);
    }

    #[test]
    fn fr22_the_window_menus_android_items_reach_android_and_the_settings() {
        use dsa_host_core::compositor::window_hosts::MenuAction;
        use dsa_host_core::tasks::AppSettingsPage;
        let mut manager = with_window();
        let chosen = |action| FromHost::MenuChosen { task_id: 7, action };
        report(&mut manager, chosen(MenuAction::AspectRatio));
        assert!(asked(
            &manager,
            &TaskRequest::OpenAppSettings {
                package_name: PACKAGE.into(),
                page: AppSettingsPage::AspectRatio,
            }
        ));
        report(&mut manager, chosen(MenuAction::OpenByDefault));
        assert!(asked(
            &manager,
            &TaskRequest::OpenAppSettings {
                package_name: PACKAGE.into(),
                page: AppSettingsPage::OpenByDefault,
            }
        ));
        report(&mut manager, chosen(MenuAction::AppInfo));
        assert_eq!(manager.platform().app_pages, vec![PACKAGE.to_string()]);
        // The app does not support several windows, so its menu never offers New Window.
        report(&mut manager, chosen(MenuAction::NewWindow));
        assert!(!asked(
            &manager,
            &TaskRequest::NewWindow {
                package_name: PACKAGE.into()
            }
        ));
    }

    #[test]
    fn fr22_new_window_opens_another_task_of_an_app_with_several_windows() {
        use dsa_host_core::compositor::window_hosts::MenuAction;
        let mut manager = with_window();
        let several = AppInfo {
            multiple_windows: true,
            ..app()
        };
        manager.handle(
            ManagerEvent::Task(Box::new(TaskEvent::App(several))),
            Instant::now(),
        );
        let offered = manager.platform().sent.iter().any(|(host, message)| {
            *host == HOST && *message == ToHost::AppMenu { new_window: true }
        });
        assert!(offered);
        let chosen = FromHost::MenuChosen {
            task_id: 7,
            action: MenuAction::NewWindow,
        };
        report(&mut manager, chosen);
        assert!(asked(
            &manager,
            &TaskRequest::NewWindow {
                package_name: PACKAGE.into()
            }
        ));
    }

    #[test]
    fn fr22_app_info_opens_the_settings_apps_page_at_the_app() {
        assert_eq!(
            app_page_url(PACKAGE).as_deref(),
            Some("dsa-settings://apps/com.example.notes")
        );
        assert_eq!(app_page_url("../evil"), None);
        assert_eq!(app_page_url("notes"), None);
    }

    #[test]
    fn fr22_the_windows_title_bar_band_reaches_the_guest() {
        use dsa_host_core::compositor::caption::CaptionBand;
        let mut manager = with_window();
        let band = CaptionBand {
            height: 56,
            start_width: 154,
            end_width: 0,
        };
        report(&mut manager, FromHost::Caption { task_id: 7, band });
        assert!(asked(
            &manager,
            &TaskRequest::SetCaption {
                task_id: 7,
                height: 56,
                start_width: 154,
                end_width: 0,
            }
        ));
    }

    #[test]
    fn fr3_finishing_lets_every_window_host_go() {
        let mut manager = with_window();
        manager.finish();
        assert!(manager.platform().routes.is_empty());
    }
}
