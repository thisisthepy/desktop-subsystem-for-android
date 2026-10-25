//! Which app window host shows which task window, and what passes between the window
//! manager and the window hosts.
//!
//! The window manager owns no window. Each app's task windows belong to a small program of
//! the app's own, its window host, which carries the app's name and icon so the desktop shows
//! the app as itself. [`WindowHosts`] turns the window changes `TaskWindows` decides into
//! messages for the right window host: it says when a window host must be started, holds the
//! messages for one that has not connected yet, and says which windows went away when a
//! window host did. The platform starts the programs and carries the messages.
//!
//! The functions at the end run the connection's opening on any transport: the handshake,
//! then `Attach` (which app the window host shows) and `Attached` (the GPU the frames are
//! on).

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use dsa_protocol::window_host::types::{
    envelope::Body, Attach, Attached, CloseWindow, Envelope, Role, SetSize, SetTitle, ShowWindow,
};
use dsa_protocol::window_host::{
    decode, local_hello, send, window_host_connect, window_manager_accept,
};
use dsa_transport::Connection;

use super::task_windows::WindowChange;
use crate::tasks::AppInfo;

/// Whether `name` is an Android package name: two or more dot-separated parts, each an ASCII
/// letter followed by letters, digits and underscores. Only such a name becomes a file name
/// for an app's window host, so a name whose first part Windows reserves for a device
/// (`con`, `nul`, `com1` and the like) is refused too.
pub fn is_valid_package(name: &str) -> bool {
    const DEVICES: [&str; 4] = ["con", "prn", "aux", "nul"];
    let parts: Vec<&str> = name.split('.').collect();
    let first = parts[0].to_ascii_lowercase();
    let numbered_device = first.len() == 4
        && (first.starts_with("com") || first.starts_with("lpt"))
        && first.as_bytes()[3].is_ascii_digit();
    name.len() <= 255
        && parts.len() >= 2
        && !DEVICES.contains(&first.as_str())
        && !numbered_device
        && parts.iter().all(|part| {
            part.bytes().next().is_some_and(|b| b.is_ascii_alphabetic())
                && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        })
}

/// Names one connection from a window host, for as long as it lasts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct HostId(pub u64);

/// What the window manager tells a window host about its windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToHost {
    Show {
        task_id: i32,
        title: String,
        width: u32,
        height: u32,
        focused: bool,
    },
    Retitle {
        task_id: i32,
        title: String,
    },
    Resize {
        task_id: i32,
        width: u32,
        height: u32,
    },
    Close {
        task_id: i32,
    },
}

impl ToHost {
    pub fn task_id(&self) -> i32 {
        match self {
            ToHost::Show { task_id, .. }
            | ToHost::Retitle { task_id, .. }
            | ToHost::Resize { task_id, .. }
            | ToHost::Close { task_id } => *task_id,
        }
    }
}

/// What a window host reports the user did to one of its windows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromHost {
    /// The user closed the window.
    Closed { task_id: i32 },
    /// The user resized the window; its client area in pixels.
    Resized {
        task_id: i32,
        width: u32,
        height: u32,
    },
    /// The window moved to a monitor of another scale.
    Dpi { task_id: i32, dpi: u32 },
    /// The window became the active window, or stopped being it.
    Focus { task_id: i32, focused: bool },
}

impl FromHost {
    pub fn task_id(&self) -> i32 {
        match self {
            FromHost::Closed { task_id }
            | FromHost::Resized { task_id, .. }
            | FromHost::Dpi { task_id, .. }
            | FromHost::Focus { task_id, .. } => *task_id,
        }
    }
}

/// What the platform must do for the window hosts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostAction {
    /// Start the window host of `package`, made with the app's name and icon when they are
    /// known.
    Start {
        package: String,
        app: Option<AppInfo>,
    },
    /// Send `message` to the window host on connection `host`.
    Send { host: HostId, message: ToHost },
    /// Ask the guest to start the app, for a window host the user started.
    Launch { package: String },
    /// Close the connection `host`; its window host then exits. `why` is for the log.
    Disconnect { host: HostId, why: String },
}

/// How long a window host may take to connect after it was started.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a window host the user started may wait for its app's first window.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

enum Link {
    /// Started at the instant, not connected yet; these messages wait for it.
    Starting {
        since: Instant,
        pending: Vec<ToHost>,
    },
    /// Connected. `waiting_since` is set while it has no window yet, for a window host the
    /// user started.
    Attached {
        host: HostId,
        waiting_since: Option<Instant>,
    },
}

struct AppHost {
    link: Link,
    /// The tasks whose windows this window host has or is about to have.
    tasks: BTreeSet<i32>,
}

/// The window hosts and the task windows each one shows.
#[derive(Default)]
pub struct WindowHosts {
    apps: BTreeMap<String, AppInfo>,
    hosts: BTreeMap<String, AppHost>,
    /// The package of each task that has a window.
    packages: BTreeMap<i32, String>,
}

impl WindowHosts {
    pub fn new() -> WindowHosts {
        WindowHosts::default()
    }

    /// Records an app's name and icon, for the next time its window host is made.
    pub fn app(&mut self, app: &AppInfo) {
        if is_valid_package(&app.package_name) {
            self.apps.insert(app.package_name.clone(), app.clone());
        }
    }

    /// The app's name and icon, when the guest gave them.
    pub fn app_info(&self, package: &str) -> Option<&AppInfo> {
        self.apps.get(package)
    }

    /// The connection of the window host that shows `task_id`'s window, once it is connected.
    pub fn host_of(&self, task_id: i32) -> Option<HostId> {
        let package = self.packages.get(&task_id)?;
        match self.hosts.get(package)?.link {
            Link::Attached { host, .. } => Some(host),
            Link::Starting { .. } => None,
        }
    }

    /// The package whose window host is on connection `host`.
    pub fn package_of(&self, host: HostId) -> Option<&str> {
        self.hosts.iter().find_map(|(package, app)| match app.link {
            Link::Attached { host: h, .. } if h == host => Some(package.as_str()),
            _ => None,
        })
    }

    /// The tasks whose windows the window host on `host` shows.
    pub fn tasks_of(&self, host: HostId) -> Vec<i32> {
        self.package_of(host)
            .and_then(|package| self.hosts.get(package))
            .map(|app| app.tasks.iter().copied().collect())
            .unwrap_or_default()
    }

    /// Carries out a window change: which window host gets which message, and which must be
    /// started first. A task of a package that cannot name a window host gets no window.
    pub fn change(&mut self, change: &WindowChange, now: Instant) -> Vec<HostAction> {
        match change {
            WindowChange::Open {
                task_id,
                package_name,
                title,
                width,
                height,
                focused,
            } => {
                if !is_valid_package(package_name) {
                    return Vec::new();
                }
                self.packages.insert(*task_id, package_name.clone());
                let message = ToHost::Show {
                    task_id: *task_id,
                    title: title.clone(),
                    width: *width,
                    height: *height,
                    focused: *focused,
                };
                let mut actions = Vec::new();
                let info = self.apps.get(package_name).cloned();
                let app = self.hosts.entry(package_name.clone()).or_insert_with(|| {
                    actions.push(HostAction::Start {
                        package: package_name.clone(),
                        app: info,
                    });
                    AppHost {
                        link: Link::Starting {
                            since: now,
                            pending: Vec::new(),
                        },
                        tasks: BTreeSet::new(),
                    }
                });
                app.tasks.insert(*task_id);
                match &mut app.link {
                    Link::Starting { pending, .. } => pending.push(message),
                    Link::Attached {
                        host,
                        waiting_since,
                    } => {
                        *waiting_since = None;
                        actions.push(HostAction::Send {
                            host: *host,
                            message,
                        });
                    }
                }
                actions
            }
            WindowChange::Retitle { task_id, title } => self.forward(ToHost::Retitle {
                task_id: *task_id,
                title: title.clone(),
            }),
            WindowChange::Resize {
                task_id,
                width,
                height,
            } => self.forward(ToHost::Resize {
                task_id: *task_id,
                width: *width,
                height: *height,
            }),
            WindowChange::Close { task_id } => self.close(*task_id),
        }
    }

    fn forward(&mut self, message: ToHost) -> Vec<HostAction> {
        let Some(package) = self.packages.get(&message.task_id()) else {
            return Vec::new();
        };
        let Some(app) = self.hosts.get_mut(package) else {
            return Vec::new();
        };
        match &mut app.link {
            Link::Starting { pending, .. } => {
                pending.push(message);
                Vec::new()
            }
            Link::Attached { host, .. } => vec![HostAction::Send {
                host: *host,
                message,
            }],
        }
    }

    fn close(&mut self, task_id: i32) -> Vec<HostAction> {
        let Some(package) = self.packages.remove(&task_id) else {
            return Vec::new();
        };
        let Some(app) = self.hosts.get_mut(&package) else {
            return Vec::new();
        };
        app.tasks.remove(&task_id);
        match &mut app.link {
            Link::Starting { pending, .. } => {
                // The window was never shown; nothing about it needs to reach the window host.
                pending.retain(|message| message.task_id() != task_id);
                Vec::new()
            }
            Link::Attached { host, .. } => {
                let host = *host;
                let mut actions = vec![HostAction::Send {
                    host,
                    message: ToHost::Close { task_id },
                }];
                if app.tasks.is_empty() {
                    // The app has no window left, so its program ends, as any app's does when
                    // its last window closes.
                    self.hosts.remove(&package);
                    actions.push(HostAction::Disconnect {
                        host,
                        why: format!("{package} has no window left"),
                    });
                }
                actions
            }
        }
    }

    /// A window host connected on `host` and said it shows `package`. `launch` says the user
    /// started it rather than the window manager.
    pub fn attached(
        &mut self,
        host: HostId,
        package: &str,
        launch: bool,
        now: Instant,
    ) -> Vec<HostAction> {
        if !is_valid_package(package) {
            return vec![HostAction::Disconnect {
                host,
                why: format!("a window host named an invalid package {package:?}"),
            }];
        }
        let launch_action = || HostAction::Launch {
            package: package.to_string(),
        };
        match self.hosts.get_mut(package) {
            Some(AppHost {
                link: Link::Attached { .. },
                ..
            }) => {
                // The app's window host runs already; a second one only asks for the app, as
                // a second start of a single-instance program does.
                let mut actions = Vec::new();
                if launch {
                    actions.push(launch_action());
                }
                actions.push(HostAction::Disconnect {
                    host,
                    why: format!("the window host of {package} runs already"),
                });
                actions
            }
            Some(app) => {
                let Link::Starting { pending, .. } = std::mem::replace(
                    &mut app.link,
                    Link::Attached {
                        host,
                        waiting_since: None,
                    },
                ) else {
                    unreachable!("matched above");
                };
                let mut actions: Vec<HostAction> = pending
                    .into_iter()
                    .map(|message| HostAction::Send { host, message })
                    .collect();
                if app.tasks.is_empty() {
                    if launch {
                        app.link = Link::Attached {
                            host,
                            waiting_since: Some(now),
                        };
                        actions.push(launch_action());
                    } else {
                        self.hosts.remove(package);
                        actions.push(HostAction::Disconnect {
                            host,
                            why: format!("{package} has no window left"),
                        });
                    }
                }
                actions
            }
            None if launch => {
                self.hosts.insert(
                    package.to_string(),
                    AppHost {
                        link: Link::Attached {
                            host,
                            waiting_since: Some(now),
                        },
                        tasks: BTreeSet::new(),
                    },
                );
                vec![launch_action()]
            }
            None => vec![HostAction::Disconnect {
                host,
                why: format!("{package} has no window to show"),
            }],
        }
    }

    /// The window host on `host` went away. Returns the tasks whose windows went with it,
    /// which count as closed by the user, as when someone ends an app from the task list.
    pub fn detached(&mut self, host: HostId) -> Vec<i32> {
        let Some(package) = self.package_of(host).map(str::to_string) else {
            return Vec::new();
        };
        let tasks = self
            .hosts
            .remove(&package)
            .map(|app| app.tasks)
            .unwrap_or_default();
        for task_id in &tasks {
            self.packages.remove(task_id);
        }
        tasks.into_iter().collect()
    }

    /// The window host of `package` could not be started. Its windows are dropped; the next
    /// window of the app tries again. Returns the tasks that got no window.
    pub fn start_failed(&mut self, package: &str) -> Vec<i32> {
        match self.hosts.get(package) {
            Some(AppHost {
                link: Link::Starting { .. },
                ..
            }) => {}
            _ => return Vec::new(),
        }
        let tasks = self
            .hosts
            .remove(package)
            .map(|app| app.tasks)
            .unwrap_or_default();
        for task_id in &tasks {
            self.packages.remove(task_id);
        }
        tasks.into_iter().collect()
    }

    /// Gives up on window hosts that did not connect in [`START_TIMEOUT`], and ends the ones
    /// the user started whose app showed no window in [`LAUNCH_TIMEOUT`]. Returns the
    /// packages that did not start, with the tasks that got no window, and the
    /// disconnections to make.
    pub fn tick(&mut self, now: Instant) -> (Vec<(String, Vec<i32>)>, Vec<HostAction>) {
        let late: Vec<String> = self
            .hosts
            .iter()
            .filter_map(|(package, app)| match app.link {
                Link::Starting { since, .. } if now.duration_since(since) >= START_TIMEOUT => {
                    Some(package.clone())
                }
                _ => None,
            })
            .collect();
        let failed = late
            .into_iter()
            .map(|package| {
                let tasks = self.start_failed(&package);
                (package, tasks)
            })
            .collect();
        let idle: Vec<(String, HostId)> = self
            .hosts
            .iter()
            .filter_map(|(package, app)| match app.link {
                Link::Attached {
                    host,
                    waiting_since: Some(since),
                } if app.tasks.is_empty() && now.duration_since(since) >= LAUNCH_TIMEOUT => {
                    Some((package.clone(), host))
                }
                _ => None,
            })
            .collect();
        let mut actions = Vec::new();
        for (package, host) in idle {
            self.hosts.remove(&package);
            actions.push(HostAction::Disconnect {
                host,
                why: format!("{package} showed no window"),
            });
        }
        (failed, actions)
    }

    /// Checks a report from the window host on `host`: it is passed on only when that window
    /// host shows the task it names.
    pub fn from_host(&self, host: HostId, report: FromHost) -> Option<FromHost> {
        (self.host_of(report.task_id()) == Some(host)).then_some(report)
    }

    /// Forgets every window host, as when the window manager stops. Returns the
    /// disconnections to make.
    pub fn reset(&mut self) -> Vec<HostAction> {
        let actions = self
            .hosts
            .values()
            .filter_map(|app| match app.link {
                Link::Attached { host, .. } => Some(HostAction::Disconnect {
                    host,
                    why: "the window manager is stopping".into(),
                }),
                Link::Starting { .. } => None,
            })
            .collect();
        self.hosts.clear();
        self.packages.clear();
        actions
    }
}

/// The frame body that carries `message`.
pub fn to_wire(message: &ToHost) -> Body {
    match message.clone() {
        ToHost::Show {
            task_id,
            title,
            width,
            height,
            focused,
        } => Body::ShowWindow(ShowWindow {
            task_id,
            title,
            width,
            height,
            focused,
        }),
        ToHost::Retitle { task_id, title } => Body::SetTitle(SetTitle { task_id, title }),
        ToHost::Resize {
            task_id,
            width,
            height,
        } => Body::SetSize(SetSize {
            task_id,
            width,
            height,
        }),
        ToHost::Close { task_id } => Body::CloseWindow(CloseWindow { task_id }),
    }
}

/// The window message a frame from the window manager carries, if it is one.
pub fn to_host_from_wire(body: &Body) -> Option<ToHost> {
    Some(match body.clone() {
        Body::ShowWindow(show) => ToHost::Show {
            task_id: show.task_id,
            title: show.title,
            width: show.width,
            height: show.height,
            focused: show.focused,
        },
        Body::SetTitle(set) => ToHost::Retitle {
            task_id: set.task_id,
            title: set.title,
        },
        Body::SetSize(set) => ToHost::Resize {
            task_id: set.task_id,
            width: set.width,
            height: set.height,
        },
        Body::CloseWindow(close) => ToHost::Close {
            task_id: close.task_id,
        },
        _ => return None,
    })
}

/// The frame body that carries `report`.
pub fn report_to_wire(report: &FromHost) -> Body {
    use dsa_protocol::window_host::types::{WindowClosed, WindowDpi, WindowFocus, WindowResized};
    match *report {
        FromHost::Closed { task_id } => Body::WindowClosed(WindowClosed { task_id }),
        FromHost::Resized {
            task_id,
            width,
            height,
        } => Body::WindowResized(WindowResized {
            task_id,
            width,
            height,
        }),
        FromHost::Dpi { task_id, dpi } => Body::WindowDpi(WindowDpi { task_id, dpi }),
        FromHost::Focus { task_id, focused } => Body::WindowFocus(WindowFocus { task_id, focused }),
    }
}

/// The report a frame from a window host carries, if it is one.
pub fn report_from_wire(body: &Body) -> Option<FromHost> {
    Some(match *body {
        Body::WindowClosed(ref closed) => FromHost::Closed {
            task_id: closed.task_id,
        },
        Body::WindowResized(ref resized) => FromHost::Resized {
            task_id: resized.task_id,
            width: resized.width,
            height: resized.height,
        },
        Body::WindowDpi(ref dpi) => FromHost::Dpi {
            task_id: dpi.task_id,
            dpi: dpi.dpi,
        },
        Body::WindowFocus(ref focus) => FromHost::Focus {
            task_id: focus.task_id,
            focused: focus.focused,
        },
        _ => return None,
    })
}

/// The window manager's side of a new connection: the handshake, then the window host's
/// `Attach`. The caller answers with [`answer_attached`] once it has taken the window host
/// on. An error says, in words for the log, why the connection is of no use.
pub fn accept_window_host(
    connection: &mut dyn Connection,
    build: &str,
    timeout: Duration,
) -> Result<Attach, String> {
    window_manager_accept(
        connection,
        &local_hello(Role::WindowManager, build),
        timeout,
    )
    .map_err(|error| error.to_string())?;
    let frame = connection
        .recv(Some(timeout))
        .map_err(|error| format!("the window host did not say which app it shows: {error}"))?;
    match decode(&frame) {
        Ok(Envelope {
            body: Some(Body::Attach(attach)),
        }) => Ok(attach),
        Ok(_) => Err("the window host sent something other than Attach".into()),
        Err(error) => Err(error.to_string()),
    }
}

/// Answers a window host's `Attach`: the frames are on the GPU `adapter_luid` names.
pub fn answer_attached(
    connection: &mut dyn Connection,
    adapter_luid: Option<[u8; 8]>,
) -> Result<(), String> {
    send(
        connection,
        Body::Attached(Attached {
            adapter_luid: adapter_luid.map(|luid| luid.to_vec()).unwrap_or_default(),
        }),
    )
    .map_err(|error| error.to_string())
}

/// The window host's side of a new connection: the handshake, then `attach`. Returns the
/// GPU the window manager's frames are on, when it has one.
pub fn connect_window_host(
    connection: &mut dyn Connection,
    build: &str,
    attach: Attach,
    timeout: Duration,
) -> Result<Option<[u8; 8]>, String> {
    window_host_connect(connection, &local_hello(Role::WindowHost, build), timeout)
        .map_err(|error| error.to_string())?;
    send(connection, Body::Attach(attach)).map_err(|error| error.to_string())?;
    let frame = connection
        .recv(Some(timeout))
        .map_err(|error| format!("the window manager did not answer: {error}"))?;
    match decode(&frame) {
        Ok(Envelope {
            body: Some(Body::Attached(attached)),
        }) => Ok(<[u8; 8]>::try_from(attached.adapter_luid.as_slice()).ok()),
        Ok(_) => Err("the window manager did not take the window host on".into()),
        Err(error) => Err(error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn open(task_id: i32, package: &str) -> WindowChange {
        WindowChange::Open {
            task_id,
            package_name: package.into(),
            title: format!("Task {task_id}"),
            width: 400,
            height: 300,
            focused: false,
        }
    }

    fn show(task_id: i32) -> ToHost {
        ToHost::Show {
            task_id,
            title: format!("Task {task_id}"),
            width: 400,
            height: 300,
            focused: false,
        }
    }

    const CLOCK: &str = "com.example.clock";
    const MAPS: &str = "com.example.maps";

    #[test]
    fn fr3_each_app_gets_its_own_window_host() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        let clock = AppInfo {
            package_name: CLOCK.into(),
            label: "Clock".into(),
            icon_png: vec![1],
        };
        hosts.app(&clock);
        assert_eq!(
            hosts.change(&open(1, CLOCK), now),
            vec![HostAction::Start {
                package: CLOCK.into(),
                app: Some(clock)
            }]
        );
        assert_eq!(
            hosts.change(&open(2, MAPS), now),
            vec![HostAction::Start {
                package: MAPS.into(),
                app: None
            }]
        );
        // A second task of the same app goes to the same window host.
        assert!(hosts.change(&open(3, CLOCK), now).is_empty());
    }

    #[test]
    fn fr3_windows_wait_for_their_window_host_to_connect() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.change(&open(1, CLOCK), now);
        hosts.change(
            &WindowChange::Retitle {
                task_id: 1,
                title: "Alarm".into(),
            },
            now,
        );
        assert_eq!(hosts.host_of(1), None);
        let host = HostId(7);
        assert_eq!(
            hosts.attached(host, CLOCK, false, now),
            vec![
                HostAction::Send {
                    host,
                    message: show(1)
                },
                HostAction::Send {
                    host,
                    message: ToHost::Retitle {
                        task_id: 1,
                        title: "Alarm".into()
                    }
                },
            ]
        );
        assert_eq!(hosts.host_of(1), Some(host));
        assert_eq!(
            hosts.change(&open(2, CLOCK), now),
            vec![HostAction::Send {
                host,
                message: show(2)
            }]
        );
        assert_eq!(hosts.tasks_of(host), vec![1, 2]);
    }

    #[test]
    fn fr3_a_window_host_ends_with_its_apps_last_window() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.change(&open(1, CLOCK), now);
        hosts.change(&open(2, CLOCK), now);
        let host = HostId(1);
        hosts.attached(host, CLOCK, false, now);
        assert_eq!(
            hosts.change(&WindowChange::Close { task_id: 1 }, now),
            vec![HostAction::Send {
                host,
                message: ToHost::Close { task_id: 1 }
            }]
        );
        let last = hosts.change(&WindowChange::Close { task_id: 2 }, now);
        assert_eq!(
            last[0],
            HostAction::Send {
                host,
                message: ToHost::Close { task_id: 2 }
            }
        );
        assert!(matches!(last[1], HostAction::Disconnect { host: h, .. } if h == host));
        // The app's next window starts a new window host.
        assert!(matches!(
            hosts.change(&open(3, CLOCK), now)[..],
            [HostAction::Start { .. }]
        ));
    }

    #[test]
    fn fr3_a_window_closed_before_its_host_connects_is_never_shown() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.change(&open(1, CLOCK), now);
        hosts.change(&WindowChange::Close { task_id: 1 }, now);
        let actions = hosts.attached(HostId(1), CLOCK, false, now);
        assert!(matches!(actions[..], [HostAction::Disconnect { .. }]));
    }

    #[test]
    fn fr3_ending_a_window_host_closes_its_windows() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.change(&open(1, CLOCK), now);
        hosts.change(&open(2, CLOCK), now);
        hosts.change(&open(3, MAPS), now);
        hosts.attached(HostId(1), CLOCK, false, now);
        hosts.attached(HostId(2), MAPS, false, now);
        assert_eq!(hosts.detached(HostId(1)), vec![1, 2]);
        assert_eq!(hosts.host_of(1), None);
        assert_eq!(hosts.host_of(3), Some(HostId(2)));
        assert!(hosts.detached(HostId(1)).is_empty());
    }

    #[test]
    fn fr3_a_window_host_reports_only_on_its_own_windows() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.change(&open(1, CLOCK), now);
        hosts.change(&open(2, MAPS), now);
        hosts.attached(HostId(1), CLOCK, false, now);
        hosts.attached(HostId(2), MAPS, false, now);
        let closed = FromHost::Closed { task_id: 1 };
        assert_eq!(hosts.from_host(HostId(1), closed.clone()), Some(closed));
        assert_eq!(
            hosts.from_host(HostId(1), FromHost::Closed { task_id: 2 }),
            None
        );
    }

    #[test]
    fn fr7_a_window_host_the_user_starts_launches_its_app() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        let host = HostId(4);
        assert_eq!(
            hosts.attached(host, CLOCK, true, now),
            vec![HostAction::Launch {
                package: CLOCK.into()
            }]
        );
        // The app's task arrives and its window goes to the waiting window host.
        assert_eq!(
            hosts.change(&open(1, CLOCK), now),
            vec![HostAction::Send {
                host,
                message: show(1)
            }]
        );
        // A second start while it runs asks for the app again and ends the newcomer.
        let second = hosts.attached(HostId(5), CLOCK, true, now);
        assert_eq!(
            second[0],
            HostAction::Launch {
                package: CLOCK.into()
            }
        );
        assert!(matches!(
            second[1],
            HostAction::Disconnect {
                host: HostId(5),
                ..
            }
        ));
    }

    #[test]
    fn fr7_a_window_host_whose_app_shows_no_window_ends() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.attached(HostId(4), CLOCK, true, now);
        let (_, actions) = hosts.tick(now + LAUNCH_TIMEOUT / 2);
        assert!(actions.is_empty());
        let (_, actions) = hosts.tick(now + LAUNCH_TIMEOUT);
        assert!(matches!(
            actions[..],
            [HostAction::Disconnect {
                host: HostId(4),
                ..
            }]
        ));
    }

    #[test]
    fn nfr5_a_window_host_that_never_connects_is_given_up_and_tried_again() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.change(&open(1, CLOCK), now);
        let (failed, _) = hosts.tick(now + START_TIMEOUT);
        assert_eq!(failed, vec![(CLOCK.to_string(), vec![1])]);
        assert!(matches!(
            hosts.change(&open(2, CLOCK), now + START_TIMEOUT)[..],
            [HostAction::Start { .. }]
        ));
        assert_eq!(hosts.start_failed(CLOCK), vec![2]);
    }

    #[test]
    fn nfr5_a_window_host_cannot_name_a_package_that_is_not_one() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        for bad in [
            "",
            "..\\evil",
            "a..b",
            "com.example/x",
            "C:\\x",
            "settings",
            "con.example",
            "COM1.x",
            "1password.app",
        ] {
            assert!(!is_valid_package(bad), "{bad}");
            assert!(matches!(
                hosts.attached(HostId(1), bad, true, now)[..],
                [HostAction::Disconnect { .. }]
            ));
            assert!(hosts.change(&open(1, bad), now).is_empty());
        }
        assert!(is_valid_package("com.android.settings"));
        assert!(is_valid_package("org.example_2.app"));
        assert!(is_valid_package("com.example.app1"));
    }

    #[test]
    fn fr3_window_messages_survive_the_wire() {
        for message in [
            show(1),
            ToHost::Retitle {
                task_id: 2,
                title: "Alarm".into(),
            },
            ToHost::Resize {
                task_id: 3,
                width: 10,
                height: 20,
            },
            ToHost::Close { task_id: 4 },
        ] {
            assert_eq!(to_host_from_wire(&to_wire(&message)), Some(message));
        }
        for report in [
            FromHost::Closed { task_id: 1 },
            FromHost::Resized {
                task_id: 2,
                width: 3,
                height: 4,
            },
            FromHost::Dpi {
                task_id: 5,
                dpi: 192,
            },
            FromHost::Focus {
                task_id: 6,
                focused: true,
            },
        ] {
            assert_eq!(report_from_wire(&report_to_wire(&report)), Some(report));
        }
    }

    #[test]
    fn fr3_a_window_host_attaches_after_the_handshake() {
        use dsa_transport::{memory, Connector, Listener};
        let (mut listener, mut connector) = memory::pair();
        let mut host_end = connector.connect(Duration::from_secs(5)).unwrap();
        let mut manager_end = listener.accept(Duration::from_secs(5)).unwrap();
        let manager = std::thread::spawn(move || {
            let attach =
                accept_window_host(&mut *manager_end, "wm", Duration::from_secs(5)).unwrap();
            answer_attached(&mut *manager_end, Some([1, 2, 3, 4, 5, 6, 7, 8])).unwrap();
            attach
        });
        let luid = connect_window_host(
            &mut *host_end,
            "host",
            Attach {
                package_name: CLOCK.into(),
                process_id: 42,
                launch: true,
            },
            Duration::from_secs(5),
        )
        .unwrap();
        assert_eq!(luid, Some([1, 2, 3, 4, 5, 6, 7, 8]));
        let attach = manager.join().unwrap();
        assert_eq!(attach.package_name, CLOCK);
        assert_eq!(attach.process_id, 42);
        assert!(attach.launch);
    }
}
