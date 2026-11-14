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

use super::caption::{CaptionBand, CaptionTone};
use super::task_windows::WindowChange;
use super::window_mode::WindowMode;
use crate::background::{MediaCommand, MediaSession, Playback};
use crate::tasks::{AppInfo, AppSettingsPage, Bounds, TaskRequest};

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
    /// The task's size in the guest and the largest and smallest client areas the window may
    /// have, and how large the window shows each task pixel (window pixels per task pixels).
    TaskSize {
        task_id: i32,
        width: u32,
        height: u32,
        max: Option<(u32, u32)>,
        min: Option<(u32, u32)>,
        scale: (u32, u32),
    },
    /// Where the cursor of the task's focused editor is, in the window's client pixels.
    ImeCursor {
        task_id: i32,
        cursor: crate::ime::ImeCursor,
    },
    /// The editor focused in the task, as the window's input method reads it.
    ImeEditor {
        task_id: i32,
        editor: crate::ime::ImeEditor,
    },
    /// The tone of the window's caption buttons, which follows the app's status bar; `None`
    /// when the app expressed nothing, so the window reads it from the pixels under its band.
    /// On the wire, `None` is the tone left unspecified.
    CaptionTone {
        task_id: i32,
        tone: Option<CaptionTone>,
    },
    /// How the window is shown, and whether the user may resize it.
    Mode {
        task_id: i32,
        mode: WindowMode,
        resizable: bool,
    },
    /// Whether the app draws its own title bar, and where its controls are in the band, in
    /// client pixels.
    CaptionMode {
        task_id: i32,
        app_drawn: bool,
        interactive: Vec<Bounds>,
    },
    /// Whether the title bar shows a back arrow in the place of the app's icon.
    CaptionBack {
        task_id: i32,
        back: bool,
    },
    /// The window opened for task `task_id` shows task `shown_task_id` from now on, which
    /// took it over after a GPU reset. Messages still name the window by `task_id`; what the
    /// window sends Android itself goes to `shown_task_id` (see [`super::window_tasks`]).
    ShownTask {
        task_id: i32,
        shown_task_id: i32,
    },
    /// What every window of the app offers in its system menu: New window only when the app
    /// supports several windows. It is about the app, not one window, so it names no task.
    AppMenu {
        new_window: bool,
    },
    /// The app's icon changed since its window host's program was made with it: every window
    /// of the app shows this one at once. `caption_icon_png` is the title bar's (an adaptive
    /// icon's foreground), empty when the icon is not adaptive. About the app, like `AppMenu`.
    AppIcon {
        icon_png: Vec<u8>,
        caption_icon_png: Vec<u8>,
    },
    /// The app's media session, for the media buttons in the taskbar preview of each of its
    /// windows; `None` once the app has none, which hides them. About the app, like `AppMenu`.
    AppMedia {
        media: Option<MediaButtons>,
    },
}

/// What the media buttons in the taskbar preview of an app's windows show: whether the app
/// plays now, and which of the buttons its media session allows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct MediaButtons {
    pub playing: bool,
    pub previous: bool,
    pub play: bool,
    pub pause: bool,
    pub next: bool,
}

impl MediaButtons {
    /// The buttons of `session`. Play and pause follow the same rule as Windows' media keys:
    /// the one the middle button shows is always offered, since a session that plays can
    /// always be paused and a paused one played, even when it lists only the other.
    pub fn of(session: &MediaSession) -> MediaButtons {
        let playing = session.playback == Playback::Playing;
        MediaButtons {
            playing,
            previous: session.can_previous,
            play: session.can_play || !playing,
            pause: session.can_pause || playing,
            next: session.can_next,
        }
    }

    /// Whether the session allows `button` now. The middle button is play or pause, as the
    /// session plays or not, so the other of the two is never pressed.
    pub fn allows(&self, button: MediaButton) -> bool {
        match button {
            MediaButton::Previous => self.previous,
            MediaButton::Play => !self.playing && self.play,
            MediaButton::Pause => self.playing && self.pause,
            MediaButton::Next => self.next,
        }
    }
}

/// A media button in the taskbar preview of a task window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MediaButton {
    Previous,
    Play,
    Pause,
    Next,
}

impl MediaButton {
    /// What the button asks of the app's media session.
    pub fn command(self) -> MediaCommand {
        match self {
            MediaButton::Previous => MediaCommand::Previous,
            MediaButton::Play => MediaCommand::Play,
            MediaButton::Pause => MediaCommand::Pause,
            MediaButton::Next => MediaCommand::Next,
        }
    }
}

impl ToHost {
    /// The task whose window the message is about; `None` for a message about the app.
    pub fn task_id(&self) -> Option<i32> {
        Some(match self {
            ToHost::AppMenu { .. } | ToHost::AppIcon { .. } | ToHost::AppMedia { .. } => {
                return None
            }
            ToHost::Show { task_id, .. }
            | ToHost::Retitle { task_id, .. }
            | ToHost::Resize { task_id, .. }
            | ToHost::TaskSize { task_id, .. }
            | ToHost::ImeCursor { task_id, .. }
            | ToHost::ImeEditor { task_id, .. }
            | ToHost::CaptionTone { task_id, .. }
            | ToHost::Mode { task_id, .. }
            | ToHost::CaptionMode { task_id, .. }
            | ToHost::CaptionBack { task_id, .. }
            | ToHost::ShownTask { task_id, .. }
            | ToHost::Close { task_id } => *task_id,
        })
    }
}

/// An item of a task window's system menu that the window manager carries out. The window
/// host takes the Screenshot itself, since it has the window's frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MenuAction {
    /// Start another task of the app in a window of its own.
    NewWindow,
    /// Open Android's setting of the app's aspect ratio.
    AspectRatio,
    /// Open Android's setting of whether the app opens the links it supports.
    OpenByDefault,
    /// Open the app's page in DSA's settings.
    AppInfo,
}

/// What the window manager does for a choice in a task window's system menu.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MenuWork {
    /// Ask the guest for this.
    Request(TaskRequest),
    /// Open DSA's settings on the page of the app `package`.
    AppPage { package: String },
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
    /// The user started dragging the window, to move or resize it, or let go.
    Dragging { task_id: i32, dragging: bool },
    /// The window was minimized (`shown` false), or shown again after it was.
    Shown { task_id: i32, shown: bool },
    /// The window draws its title bar over its top `band.height` client pixels, with its
    /// icon's cell and its caption buttons reaching `band.start_width` and `band.end_width`
    /// in from the ends.
    Caption { task_id: i32, band: CaptionBand },
    /// The user chose `action` in the window's system menu.
    MenuChosen { task_id: i32, action: MenuAction },
    /// The user clicked `button` in the taskbar preview of the window.
    MediaButton { task_id: i32, button: MediaButton },
}

impl FromHost {
    pub fn task_id(&self) -> i32 {
        match self {
            FromHost::MediaButton { task_id, .. }
            | FromHost::Closed { task_id }
            | FromHost::Resized { task_id, .. }
            | FromHost::Dpi { task_id, .. }
            | FromHost::Focus { task_id, .. }
            | FromHost::Dragging { task_id, .. }
            | FromHost::Shown { task_id, .. }
            | FromHost::Caption { task_id, .. }
            | FromHost::MenuChosen { task_id, .. } => *task_id,
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
        app: Option<Box<AppInfo>>,
    },
    /// Send `message` to the window host on connection `host`.
    Send { host: HostId, message: ToHost },
    /// Ask the guest to start the app, for a window host the user started.
    /// `shortcut` names one of the app's shortcuts to start instead, for a window host started
    /// from a jump list task or a desktop shortcut.
    Launch {
        package: String,
        shortcut: Option<String>,
    },
    /// Close the connection `host`; its window host then exits. `why` is for the log.
    Disconnect { host: HostId, why: String },
}

/// How long a window host may take to connect after it was started.
pub const START_TIMEOUT: Duration = Duration::from_secs(10);

/// How long the start of a window host waits for its app's name and icon when the guest has
/// not given them yet. The window host's program is made with the app's name, which Task
/// Manager and the taskbar show; started without it, the program would carry the package
/// name until its windows close.
pub const APP_INFO_WAIT: Duration = Duration::from_millis(1500);

/// How long a window host the user started may wait for its app's first window.
pub const LAUNCH_TIMEOUT: Duration = Duration::from_secs(30);

enum Link {
    /// Not connected yet; these messages wait for it. `named` is false while the start
    /// waits for the app's name and icon, since the instant; true once the window host was
    /// started, at the instant.
    Starting {
        since: Instant,
        named: bool,
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
    /// The icon the window host shows: the one its program was made with, or the one it was
    /// last sent. `None` while it is not known, as for a window host the user started.
    icon: Option<Icon>,
}

/// An app's icon and its title bar's icon, as PNG images.
type Icon = (Vec<u8>, Vec<u8>);

/// The icon of `app`, when the guest drew one.
fn icon_of(app: &AppInfo) -> Option<Icon> {
    (!app.icon_png.is_empty()).then(|| (app.icon_png.clone(), app.caption_icon_png.clone()))
}

/// The message that shows `icon` on every window of an app.
fn icon_message(host: HostId, icon: &Icon) -> HostAction {
    HostAction::Send {
        host,
        message: ToHost::AppIcon {
            icon_png: icon.0.clone(),
            caption_icon_png: icon.1.clone(),
        },
    }
}

/// The window hosts and the task windows each one shows.
#[derive(Default)]
pub struct WindowHosts {
    apps: BTreeMap<String, AppInfo>,
    hosts: BTreeMap<String, AppHost>,
    /// The package of each task that has a window.
    packages: BTreeMap<i32, String>,
    /// The media buttons of each app that has a media session, whether or not its windows
    /// are open, so a window host that attaches later shows them at once.
    media: BTreeMap<String, MediaButtons>,
}

impl WindowHosts {
    pub fn new() -> WindowHosts {
        WindowHosts::default()
    }

    /// Records the media session of `package`, or its end (`None`), and tells the app's
    /// window host when it runs and the buttons it shows change.
    pub fn media(&mut self, package: &str, media: Option<MediaButtons>) -> Vec<HostAction> {
        let before = match media {
            Some(buttons) => self.media.insert(package.to_string(), buttons),
            None => self.media.remove(package),
        };
        if before == media {
            return Vec::new();
        }
        match self.hosts.get(package).map(|app| &app.link) {
            Some(Link::Attached { host, .. }) => vec![HostAction::Send {
                host: *host,
                message: ToHost::AppMedia { media },
            }],
            // A window host that has not attached hears of it when it does.
            _ => Vec::new(),
        }
    }

    /// The message that shows the media buttons of `package` on its window host's windows,
    /// when the app has a media session.
    fn media_message(&self, host: HostId, package: &str) -> Option<HostAction> {
        let media = *self.media.get(package)?;
        Some(HostAction::Send {
            host,
            message: ToHost::AppMedia { media: Some(media) },
        })
    }

    /// What the media button `button`, clicked in the taskbar preview of `task_id`'s window,
    /// asks of which app's media session. `None` for a window it does not know, an app with
    /// no media session, and a button the session does not allow now.
    pub fn media_pressed(
        &self,
        task_id: i32,
        button: MediaButton,
    ) -> Option<(String, MediaCommand)> {
        let package = self.packages.get(&task_id)?;
        let media = self.media.get(package)?;
        media
            .allows(button)
            .then(|| (package.clone(), button.command()))
    }

    /// Records an app's name and icon, for the next time its window host is made. A window
    /// host that waits for them is started now; one that runs is told of a new icon.
    pub fn app(&mut self, app: &AppInfo, now: Instant) -> Vec<HostAction> {
        if !is_valid_package(&app.package_name) {
            return Vec::new();
        }
        let offered = self.offers_new_window(&app.package_name);
        self.apps.insert(app.package_name.clone(), app.clone());
        let Some(host) = self.hosts.get_mut(&app.package_name) else {
            return Vec::new();
        };
        match &mut host.link {
            Link::Attached { host: id, .. } => {
                let id = *id;
                let mut actions = Vec::new();
                // A window host that runs hears when its app starts or stops supporting
                // several windows, as an update of the app may change it.
                if offered != app.multiple_windows {
                    actions.push(HostAction::Send {
                        host: id,
                        message: ToHost::AppMenu {
                            new_window: app.multiple_windows,
                        },
                    });
                }
                // Its windows show a new icon at once; its program takes it once it ends.
                if let Some(icon) = icon_of(app).filter(|icon| host.icon.as_ref() != Some(icon)) {
                    actions.push(icon_message(id, &icon));
                    host.icon = Some(icon);
                }
                actions
            }
            Link::Starting { since, named, .. } if !*named => {
                *named = true;
                *since = now;
                host.icon = icon_of(app);
                vec![HostAction::Start {
                    package: app.package_name.clone(),
                    app: Some(Box::new(app.clone())),
                }]
            }
            // Started already with what was known then: it hears of the new icon when it
            // attaches.
            Link::Starting { .. } => Vec::new(),
        }
    }

    /// The app's name and icon, when the guest gave them.
    pub fn app_info(&self, package: &str) -> Option<&AppInfo> {
        self.apps.get(package)
    }

    /// Whether the system menu of the app's windows offers New window: only when the guest
    /// said the app supports several windows.
    fn offers_new_window(&self, package: &str) -> bool {
        self.apps
            .get(package)
            .is_some_and(|app| app.multiple_windows)
    }

    /// The menu of the app `package`'s windows, for its window host once it attaches.
    fn app_menu(&self, host: HostId, package: &str) -> HostAction {
        HostAction::Send {
            host,
            message: ToHost::AppMenu {
                new_window: self.offers_new_window(package),
            },
        }
    }

    /// What the window manager does for `action`, chosen in the system menu of `task_id`'s
    /// window. `None` for a window it does not know, and for New window of an app that does
    /// not support several windows, which its menu never offers.
    pub fn menu_chosen(&self, task_id: i32, action: MenuAction) -> Option<MenuWork> {
        let package = self.packages.get(&task_id)?.clone();
        let settings = |page| {
            MenuWork::Request(TaskRequest::OpenAppSettings {
                package_name: package.clone(),
                page,
            })
        };
        Some(match action {
            MenuAction::NewWindow if self.offers_new_window(&package) => {
                MenuWork::Request(TaskRequest::NewWindow {
                    package_name: package.clone(),
                })
            }
            MenuAction::NewWindow => return None,
            MenuAction::AspectRatio => settings(AppSettingsPage::AspectRatio),
            MenuAction::OpenByDefault => settings(AppSettingsPage::OpenByDefault),
            MenuAction::AppInfo => MenuWork::AppPage {
                package: package.clone(),
            },
        })
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
                    // Without the app's name yet, the start waits for it a little.
                    let named = info.is_some();
                    let icon = info.as_ref().and_then(icon_of);
                    if named {
                        actions.push(HostAction::Start {
                            package: package_name.clone(),
                            app: info.map(Box::new),
                        });
                    }
                    AppHost {
                        link: Link::Starting {
                            since: now,
                            named,
                            pending: Vec::new(),
                        },
                        tasks: BTreeSet::new(),
                        icon,
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
            WindowChange::TaskSize {
                task_id,
                width,
                height,
                max,
                min,
                scale,
            } => self.forward(ToHost::TaskSize {
                task_id: *task_id,
                width: *width,
                height: *height,
                max: *max,
                min: *min,
                scale: *scale,
            }),
            WindowChange::ImeCursor { task_id, cursor } => self.forward(ToHost::ImeCursor {
                task_id: *task_id,
                cursor: *cursor,
            }),
            WindowChange::ImeEditor { task_id, editor } => self.forward(ToHost::ImeEditor {
                task_id: *task_id,
                editor: editor.clone(),
            }),
            WindowChange::CaptionTone { task_id, tone } => self.forward(ToHost::CaptionTone {
                task_id: *task_id,
                tone: *tone,
            }),
            WindowChange::Mode {
                task_id,
                mode,
                resizable,
            } => self.forward(ToHost::Mode {
                task_id: *task_id,
                mode: *mode,
                resizable: *resizable,
            }),
            WindowChange::CaptionMode {
                task_id,
                app_drawn,
                interactive,
            } => self.forward(ToHost::CaptionMode {
                task_id: *task_id,
                app_drawn: *app_drawn,
                interactive: interactive.clone(),
            }),
            WindowChange::CaptionBack { task_id, back } => self.forward(ToHost::CaptionBack {
                task_id: *task_id,
                back: *back,
            }),
            WindowChange::ShownTask {
                task_id,
                shown_task_id,
            } => self.forward(ToHost::ShownTask {
                task_id: *task_id,
                shown_task_id: *shown_task_id,
            }),
        }
    }

    fn forward(&mut self, message: ToHost) -> Vec<HostAction> {
        let Some(package) = message
            .task_id()
            .and_then(|task_id| self.packages.get(&task_id))
        else {
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
            Link::Starting { pending, named, .. } => {
                // The window was never shown; nothing about it needs to reach the window host.
                pending.retain(|message| message.task_id() != Some(task_id));
                if !*named && app.tasks.is_empty() {
                    // Nothing was started, so nothing is left to wait for.
                    self.hosts.remove(&package);
                }
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
        self.attached_for(host, package, launch, None, now)
    }

    /// [`WindowHosts::attached`], for a window host the user started from one of the app's
    /// shortcuts (`shortcut`, with `launch`): the shortcut is started rather than the app,
    /// whether the app's window host runs already or not. An ID no command line could carry
    /// starts the app.
    pub fn attached_for(
        &mut self,
        host: HostId,
        package: &str,
        launch: bool,
        shortcut: Option<&str>,
        now: Instant,
    ) -> Vec<HostAction> {
        if !is_valid_package(package) {
            return vec![HostAction::Disconnect {
                host,
                why: format!("a window host named an invalid package {package:?}"),
            }];
        }
        let shortcut = shortcut
            .filter(|id| crate::tasks::is_valid_shortcut_id(id))
            .map(str::to_string);
        let launch_action = || HostAction::Launch {
            package: package.to_string(),
            shortcut: shortcut.clone(),
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
            Some(_) => {
                // The menu goes first, so the windows open with it, and the app's icon when it
                // changed after the program was made.
                let menu = self.app_menu(host, package);
                let media = self.media_message(host, package);
                let latest = self.apps.get(package).and_then(icon_of);
                let Some(app) = self.hosts.get_mut(package) else {
                    unreachable!("matched above");
                };
                let icon = latest.filter(|icon| app.icon.as_ref() != Some(icon));
                if icon.is_some() {
                    app.icon = icon.clone();
                }
                let Link::Starting { pending, .. } = std::mem::replace(
                    &mut app.link,
                    Link::Attached {
                        host,
                        waiting_since: None,
                    },
                ) else {
                    unreachable!("matched above");
                };
                let mut actions: Vec<HostAction> = std::iter::once(menu)
                    .chain(icon.map(|icon| icon_message(host, &icon)))
                    .chain(media)
                    .chain(
                        pending
                            .into_iter()
                            .map(|message| HostAction::Send { host, message }),
                    )
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
                        actions = vec![HostAction::Disconnect {
                            host,
                            why: format!("{package} has no window left"),
                        }];
                    }
                }
                actions
            }
            None if launch => {
                // The user started the program the Start menu made, whose icon may be older
                // than the one the guest gave since.
                let icon = self.apps.get(package).and_then(icon_of);
                self.hosts.insert(
                    package.to_string(),
                    AppHost {
                        link: Link::Attached {
                            host,
                            waiting_since: Some(now),
                        },
                        tasks: BTreeSet::new(),
                        icon: icon.clone(),
                    },
                );
                std::iter::once(self.app_menu(host, package))
                    .chain(icon.map(|icon| icon_message(host, &icon)))
                    .chain(self.media_message(host, package))
                    .chain(std::iter::once(launch_action()))
                    .collect()
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
    /// A window host that waited [`APP_INFO_WAIT`] for its app's name without getting it is
    /// started without it.
    pub fn tick(&mut self, now: Instant) -> (Vec<(String, Vec<i32>)>, Vec<HostAction>) {
        let mut actions = Vec::new();
        for (package, app) in self.hosts.iter_mut() {
            if let Link::Starting { since, named, .. } = &mut app.link {
                if !*named && now.duration_since(*since) >= APP_INFO_WAIT {
                    *named = true;
                    *since = now;
                    let info = self.apps.get(package).cloned();
                    app.icon = info.as_ref().and_then(icon_of);
                    actions.push(HostAction::Start {
                        package: package.clone(),
                        app: info.map(Box::new),
                    });
                }
            }
        }
        let late: Vec<String> = self
            .hosts
            .iter()
            .filter_map(|(package, app)| match app.link {
                Link::Starting {
                    since, named: true, ..
                } if now.duration_since(since) >= START_TIMEOUT => Some(package.clone()),
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
        ToHost::TaskSize {
            task_id,
            width,
            height,
            max,
            min,
            scale: (scale_numerator, scale_denominator),
        } => {
            let (max_width, max_height) = max.unwrap_or((0, 0));
            let (min_width, min_height) = min.unwrap_or((0, 0));
            Body::TaskSize(dsa_protocol::window_host::types::TaskSize {
                task_id,
                width,
                height,
                max_width,
                max_height,
                min_width,
                min_height,
                scale_numerator,
                scale_denominator,
            })
        }
        ToHost::ImeCursor { task_id, cursor } => {
            Body::ImeCursor(dsa_protocol::window_host::types::ImeCursor {
                task_id,
                left: cursor.left,
                top: cursor.top,
                right: cursor.right,
                bottom: cursor.bottom,
            })
        }
        ToHost::ImeEditor { task_id, editor } => {
            let (composition_start, composition_end) = editor
                .composition
                .unwrap_or((editor.selection.1, editor.selection.1));
            Body::ImeEditor(dsa_protocol::window_host::types::ImeEditor {
                task_id,
                kind: editor.kind.number(),
                text: editor.text,
                text_start: editor.text_start,
                selection_start: editor.selection.0,
                selection_end: editor.selection.1,
                composition_start,
                composition_end,
            })
        }
        ToHost::CaptionTone { task_id, tone } => {
            use dsa_protocol::window_host::types::{CaptionTone as WireTone, SetCaptionTone};
            let tone = match tone {
                Some(CaptionTone::DarkGlyphs) => WireTone::DarkGlyphs,
                Some(CaptionTone::LightGlyphs) => WireTone::LightGlyphs,
                None => WireTone::Unspecified,
            };
            Body::SetCaptionTone(SetCaptionTone {
                task_id,
                tone: tone as i32,
            })
        }
        ToHost::Mode {
            task_id,
            mode,
            resizable,
        } => {
            use dsa_protocol::window_host::types::{SetWindowMode, WindowMode as WireMode};
            let mode = match mode {
                WindowMode::Normal => WireMode::Normal,
                WindowMode::Fullscreen => WireMode::Fullscreen,
                WindowMode::PictureInPicture => WireMode::PictureInPicture,
            };
            Body::SetWindowMode(SetWindowMode {
                task_id,
                mode: mode as i32,
                resizable,
            })
        }
        ToHost::CaptionMode {
            task_id,
            app_drawn,
            interactive,
        } => {
            use dsa_protocol::window_host::types::{ClientRect, SetCaptionMode};
            Body::SetCaptionMode(SetCaptionMode {
                task_id,
                app_drawn,
                interactive: interactive
                    .iter()
                    .map(|area| ClientRect {
                        left: area.left,
                        top: area.top,
                        right: area.right,
                        bottom: area.bottom,
                    })
                    .collect(),
            })
        }
        ToHost::CaptionBack { task_id, back } => {
            Body::SetCaptionBack(dsa_protocol::window_host::types::SetCaptionBack { task_id, back })
        }
        ToHost::ShownTask {
            task_id,
            shown_task_id,
        } => Body::SetShownTask(dsa_protocol::window_host::types::SetShownTask {
            task_id,
            shown_task_id,
        }),
        ToHost::AppMenu { new_window } => {
            Body::AppMenu(dsa_protocol::window_host::types::AppMenu { new_window })
        }
        ToHost::AppIcon {
            icon_png,
            caption_icon_png,
        } => Body::AppIcon(dsa_protocol::window_host::types::AppIcon {
            icon_png,
            caption_icon_png,
        }),
        ToHost::AppMedia { media } => {
            let buttons = media.unwrap_or_default();
            Body::AppMedia(dsa_protocol::window_host::types::AppMedia {
                active: media.is_some(),
                playing: buttons.playing,
                can_previous: buttons.previous,
                can_play: buttons.play,
                can_pause: buttons.pause,
                can_next: buttons.next,
            })
        }
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
        Body::TaskSize(size) => ToHost::TaskSize {
            task_id: size.task_id,
            width: size.width,
            height: size.height,
            max: (size.max_width > 0 && size.max_height > 0)
                .then_some((size.max_width, size.max_height)),
            min: (size.min_width > 0 || size.min_height > 0)
                .then_some((size.min_width, size.min_height)),
            // 0 by 0, or anything with a 0 in it, is 1:1.
            scale: if size.scale_numerator > 0 && size.scale_denominator > 0 {
                (size.scale_numerator, size.scale_denominator)
            } else {
                (1, 1)
            },
        },
        Body::ImeCursor(cursor) => ToHost::ImeCursor {
            task_id: cursor.task_id,
            cursor: crate::ime::ImeCursor {
                left: cursor.left,
                top: cursor.top,
                right: cursor.right,
                bottom: cursor.bottom,
            },
        },
        // A description that makes no sense is dropped; the next one replaces it.
        Body::ImeEditor(editor) => ToHost::ImeEditor {
            task_id: editor.task_id,
            editor: crate::ime::ImeEditor::checked(
                editor.kind,
                &editor.text,
                editor.text_start,
                (editor.selection_start, editor.selection_end),
                (editor.composition_start, editor.composition_end),
            )
            .ok()?,
        },
        Body::SetCaptionTone(set) => {
            use dsa_protocol::window_host::types::CaptionTone as WireTone;
            ToHost::CaptionTone {
                task_id: set.task_id,
                // An unspecified tone, or one this build does not know, says nothing: the
                // window reads the tone from its pixels.
                tone: match set.tone() {
                    WireTone::LightGlyphs => Some(CaptionTone::LightGlyphs),
                    WireTone::DarkGlyphs => Some(CaptionTone::DarkGlyphs),
                    WireTone::Unspecified => None,
                },
            }
        }
        Body::SetWindowMode(set) => {
            use dsa_protocol::window_host::types::WindowMode as WireMode;
            ToHost::Mode {
                task_id: set.task_id,
                // A mode this build does not know reads as an ordinary window.
                mode: match set.mode() {
                    WireMode::Fullscreen => WindowMode::Fullscreen,
                    WireMode::PictureInPicture => WindowMode::PictureInPicture,
                    WireMode::Normal => WindowMode::Normal,
                },
                resizable: set.resizable,
            }
        }
        Body::SetCaptionMode(set) => ToHost::CaptionMode {
            task_id: set.task_id,
            app_drawn: set.app_drawn,
            // The controls count only while the app draws its own title bar.
            interactive: if set.app_drawn {
                set.interactive
                    .iter()
                    .map(|area| Bounds {
                        left: area.left,
                        top: area.top,
                        right: area.right,
                        bottom: area.bottom,
                    })
                    .collect()
            } else {
                Vec::new()
            },
        },
        Body::SetCaptionBack(set) => ToHost::CaptionBack {
            task_id: set.task_id,
            back: set.back,
        },
        Body::SetShownTask(set) => ToHost::ShownTask {
            task_id: set.task_id,
            shown_task_id: set.shown_task_id,
        },
        Body::AppMenu(menu) => ToHost::AppMenu {
            new_window: menu.new_window,
        },
        // An empty icon would leave the windows with none; the window keeps the one it has.
        Body::AppIcon(icon) if icon.icon_png.is_empty() => return None,
        Body::AppIcon(icon) => ToHost::AppIcon {
            icon_png: icon.icon_png,
            caption_icon_png: icon.caption_icon_png,
        },
        Body::AppMedia(media) => ToHost::AppMedia {
            media: media.active.then_some(MediaButtons {
                playing: media.playing,
                previous: media.can_previous,
                play: media.can_play,
                pause: media.can_pause,
                next: media.can_next,
            }),
        },
        _ => return None,
    })
}

/// The frame body that carries `report`.
pub fn report_to_wire(report: &FromHost) -> Body {
    use dsa_protocol::window_host::types::{
        WindowClosed, WindowDpi, WindowDragging, WindowFocus, WindowResized, WindowShown,
    };
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
        FromHost::Dragging { task_id, dragging } => {
            Body::WindowDragging(WindowDragging { task_id, dragging })
        }
        FromHost::Shown { task_id, shown } => Body::WindowShown(WindowShown { task_id, shown }),
        FromHost::Caption { task_id, band } => {
            Body::WindowCaption(dsa_protocol::window_host::types::WindowCaption {
                task_id,
                height: band.height,
                start_width: band.start_width,
                end_width: band.end_width,
            })
        }
        FromHost::MenuChosen { task_id, action } => {
            use dsa_protocol::window_host::types::{MenuAction as WireAction, MenuChosen};
            let action = match action {
                MenuAction::NewWindow => WireAction::NewWindow,
                MenuAction::AspectRatio => WireAction::AspectRatio,
                MenuAction::OpenByDefault => WireAction::OpenByDefault,
                MenuAction::AppInfo => WireAction::AppInfo,
            };
            Body::MenuChosen(MenuChosen {
                task_id,
                action: action as i32,
            })
        }
        FromHost::MediaButton { task_id, button } => {
            use dsa_protocol::window_host::types::{MediaButton as WireButton, MediaButtonPressed};
            let button = match button {
                MediaButton::Previous => WireButton::Previous,
                MediaButton::Play => WireButton::Play,
                MediaButton::Pause => WireButton::Pause,
                MediaButton::Next => WireButton::Next,
            };
            Body::MediaButtonPressed(MediaButtonPressed {
                task_id,
                button: button as i32,
            })
        }
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
        Body::WindowDragging(ref dragging) => FromHost::Dragging {
            task_id: dragging.task_id,
            dragging: dragging.dragging,
        },
        Body::WindowShown(ref shown) => FromHost::Shown {
            task_id: shown.task_id,
            shown: shown.shown,
        },
        Body::WindowCaption(ref caption) => FromHost::Caption {
            task_id: caption.task_id,
            band: CaptionBand {
                height: caption.height,
                start_width: caption.start_width,
                end_width: caption.end_width,
            },
        },
        Body::MenuChosen(ref chosen) => {
            use dsa_protocol::window_host::types::MenuAction as WireAction;
            FromHost::MenuChosen {
                task_id: chosen.task_id,
                // An item this build does not know is no choice at all.
                action: match chosen.action() {
                    WireAction::NewWindow => MenuAction::NewWindow,
                    WireAction::AspectRatio => MenuAction::AspectRatio,
                    WireAction::OpenByDefault => MenuAction::OpenByDefault,
                    WireAction::AppInfo => MenuAction::AppInfo,
                    WireAction::Unspecified => return None,
                },
            }
        }
        Body::MediaButtonPressed(ref pressed) => {
            use dsa_protocol::window_host::types::MediaButton as WireButton;
            FromHost::MediaButton {
                task_id: pressed.task_id,
                // A button this build does not know is no press at all.
                button: match pressed.button() {
                    WireButton::Previous => MediaButton::Previous,
                    WireButton::Play => MediaButton::Play,
                    WireButton::Pause => MediaButton::Pause,
                    WireButton::Next => MediaButton::Next,
                    WireButton::Unspecified => return None,
                },
            }
        }
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

    fn info(package: &str) -> AppInfo {
        AppInfo {
            package_name: package.into(),
            label: "App".into(),
            icon_png: Vec::new(),
            ..Default::default()
        }
    }

    /// The system menu a window host hears of when it attaches.
    fn menu(host: HostId, new_window: bool) -> HostAction {
        HostAction::Send {
            host,
            message: ToHost::AppMenu { new_window },
        }
    }

    /// Window hosts that know the name of every app the tests open.
    fn named_hosts(now: Instant) -> WindowHosts {
        let mut hosts = WindowHosts::new();
        hosts.app(&info(CLOCK), now);
        hosts.app(&info(MAPS), now);
        hosts
    }

    #[test]
    fn fr3_each_app_gets_its_own_window_host() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        let clock = AppInfo {
            package_name: CLOCK.into(),
            label: "Clock".into(),
            icon_png: vec![1],
            ..Default::default()
        };
        assert!(hosts.app(&clock, now).is_empty());
        assert_eq!(
            hosts.change(&open(1, CLOCK), now),
            vec![HostAction::Start {
                package: CLOCK.into(),
                app: Some(Box::new(clock))
            }]
        );
        // An app whose name is not known yet waits for it.
        assert!(hosts.change(&open(2, MAPS), now).is_empty());
        assert_eq!(
            hosts.app(&info(MAPS), now),
            vec![HostAction::Start {
                package: MAPS.into(),
                app: Some(Box::new(info(MAPS)))
            }]
        );
        // A second task of the same app goes to the same window host.
        assert!(hosts.change(&open(3, CLOCK), now).is_empty());
    }

    #[test]
    fn fr3_a_window_host_waits_for_its_apps_name_before_it_starts() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        // The task arrived before the app's name: nothing starts yet.
        assert!(hosts.change(&open(1, CLOCK), now).is_empty());
        assert_eq!(
            hosts.tick(now + APP_INFO_WAIT / 2),
            (Vec::new(), Vec::new())
        );
        // The name arrives in time: the window host starts with it, once.
        assert_eq!(
            hosts.app(&info(CLOCK), now + APP_INFO_WAIT / 2),
            vec![HostAction::Start {
                package: CLOCK.into(),
                app: Some(Box::new(info(CLOCK)))
            }]
        );
        assert!(hosts.app(&info(CLOCK), now + APP_INFO_WAIT).is_empty());
        let (_, actions) = hosts.tick(now + APP_INFO_WAIT * 2);
        assert!(actions.is_empty());
        // The window waits for the window host, as any window does.
        assert_eq!(
            hosts.attached(HostId(1), CLOCK, false, now),
            vec![
                menu(HostId(1), false),
                HostAction::Send {
                    host: HostId(1),
                    message: show(1)
                }
            ]
        );
    }

    #[test]
    fn fr3_a_window_host_whose_apps_name_never_comes_starts_without_it() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        assert!(hosts.change(&open(1, MAPS), now).is_empty());
        let (failed, actions) = hosts.tick(now + APP_INFO_WAIT);
        assert!(failed.is_empty());
        assert_eq!(
            actions,
            vec![HostAction::Start {
                package: MAPS.into(),
                app: None
            }]
        );
        // The time to connect counts from the start, not from the wait.
        let (failed, _) = hosts.tick(now + START_TIMEOUT);
        assert!(failed.is_empty());
        let (failed, _) = hosts.tick(now + APP_INFO_WAIT + START_TIMEOUT);
        assert_eq!(failed, vec![(MAPS.to_string(), vec![1])]);
    }

    #[test]
    fn fr3_a_window_closed_while_its_host_waits_for_a_name_starts_nothing() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.change(&open(1, CLOCK), now);
        assert!(hosts
            .change(&WindowChange::Close { task_id: 1 }, now)
            .is_empty());
        assert!(hosts.app(&info(CLOCK), now).is_empty());
        assert_eq!(hosts.tick(now + APP_INFO_WAIT), (Vec::new(), Vec::new()));
    }

    #[test]
    fn fr3_windows_wait_for_their_window_host_to_connect() {
        let now = Instant::now();
        let mut hosts = named_hosts(now);
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
                menu(host, false),
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
        let mut hosts = named_hosts(now);
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
        let mut hosts = named_hosts(now);
        hosts.change(&open(1, CLOCK), now);
        hosts.change(&WindowChange::Close { task_id: 1 }, now);
        let actions = hosts.attached(HostId(1), CLOCK, false, now);
        assert!(matches!(actions[..], [HostAction::Disconnect { .. }]));
    }

    #[test]
    fn fr3_ending_a_window_host_closes_its_windows() {
        let now = Instant::now();
        let mut hosts = named_hosts(now);
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
        let mut hosts = named_hosts(now);
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
            vec![
                menu(host, false),
                HostAction::Launch {
                    package: CLOCK.into(),
                    shortcut: None,
                }
            ]
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
                package: CLOCK.into(),
                shortcut: None,
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
    fn fr22_the_system_menu_carries_the_android_window_menu() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        let several = AppInfo {
            multiple_windows: true,
            ..info(CLOCK)
        };
        hosts.app(&several, now);
        hosts.app(&info(MAPS), now);
        hosts.change(&open(1, CLOCK), now);
        hosts.change(&open(2, MAPS), now);
        // Each window host hears first whether its app's windows offer New window.
        assert_eq!(
            hosts.attached(HostId(1), CLOCK, false, now)[0],
            menu(HostId(1), true)
        );
        assert_eq!(
            hosts.attached(HostId(2), MAPS, false, now)[0],
            menu(HostId(2), false)
        );
        // An app that changes says so again; the same answer says nothing.
        assert!(hosts.app(&several, now).is_empty());
        assert_eq!(hosts.app(&info(CLOCK), now), vec![menu(HostId(1), false)]);
        assert_eq!(hosts.app(&several, now), vec![menu(HostId(1), true)]);
        assert!(hosts.app(&info(MAPS), now).is_empty());

        // Each choice becomes what the window manager does for the window's app.
        assert_eq!(
            hosts.menu_chosen(1, MenuAction::NewWindow),
            Some(MenuWork::Request(TaskRequest::NewWindow {
                package_name: CLOCK.into()
            }))
        );
        // New window of an app that takes one window is never offered, so it does nothing.
        assert_eq!(hosts.menu_chosen(2, MenuAction::NewWindow), None);
        assert_eq!(
            hosts.menu_chosen(2, MenuAction::AspectRatio),
            Some(MenuWork::Request(TaskRequest::OpenAppSettings {
                package_name: MAPS.into(),
                page: AppSettingsPage::AspectRatio
            }))
        );
        assert_eq!(
            hosts.menu_chosen(2, MenuAction::OpenByDefault),
            Some(MenuWork::Request(TaskRequest::OpenAppSettings {
                package_name: MAPS.into(),
                page: AppSettingsPage::OpenByDefault
            }))
        );
        assert_eq!(
            hosts.menu_chosen(1, MenuAction::AppInfo),
            Some(MenuWork::AppPage {
                package: CLOCK.into()
            })
        );
        // A window it does not know chooses nothing.
        assert_eq!(hosts.menu_chosen(9, MenuAction::AppInfo), None);
        // A choice is a report like any other: only the window's own window host makes it.
        let chosen = FromHost::MenuChosen {
            task_id: 1,
            action: MenuAction::AppInfo,
        };
        assert_eq!(
            hosts.from_host(HostId(1), chosen.clone()),
            Some(chosen.clone())
        );
        assert_eq!(hosts.from_host(HostId(2), chosen), None);
    }

    #[test]
    fn fr22_a_menu_item_this_build_does_not_know_is_no_choice() {
        use dsa_protocol::window_host::types::MenuChosen;
        assert_eq!(
            report_from_wire(&Body::MenuChosen(MenuChosen {
                task_id: 1,
                action: 0
            })),
            None
        );
        assert_eq!(
            report_from_wire(&Body::MenuChosen(MenuChosen {
                task_id: 1,
                action: 99
            })),
            None
        );
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
        let mut hosts = named_hosts(now);
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
            ToHost::TaskSize {
                task_id: 5,
                width: 600,
                height: 400,
                max: Some((1920, 1080)),
                min: Some((700, 500)),
                scale: (1, 1),
            },
            ToHost::TaskSize {
                task_id: 6,
                width: 600,
                height: 400,
                max: None,
                min: None,
                scale: (3, 2),
            },
            ToHost::ImeCursor {
                task_id: 7,
                cursor: crate::ime::ImeCursor {
                    left: 12,
                    top: 30,
                    right: 14,
                    bottom: 60,
                },
            },
            ToHost::ImeEditor {
                task_id: 7,
                editor: crate::ime::ImeEditor {
                    kind: crate::ime::InputKind::Email,
                    text: "\u{d55c}a\u{1f600}".into(),
                    text_start: 40,
                    selection: (44, 41),
                    composition: Some((40, 41)),
                },
            },
            ToHost::ImeEditor {
                task_id: 7,
                editor: crate::ime::ImeEditor::none(),
            },
            ToHost::CaptionTone {
                task_id: 8,
                tone: Some(CaptionTone::LightGlyphs),
            },
            ToHost::CaptionTone {
                task_id: 9,
                tone: None,
            },
            ToHost::CaptionTone {
                task_id: 9,
                tone: Some(CaptionTone::DarkGlyphs),
            },
            ToHost::CaptionMode {
                task_id: 10,
                app_drawn: true,
                interactive: vec![Bounds {
                    left: 48,
                    top: 0,
                    right: 600,
                    bottom: 31,
                }],
            },
            ToHost::CaptionMode {
                task_id: 10,
                app_drawn: false,
                interactive: Vec::new(),
            },
            ToHost::CaptionBack {
                task_id: 11,
                back: true,
            },
            ToHost::ShownTask {
                task_id: 12,
                shown_task_id: 31,
            },
            ToHost::AppMenu { new_window: true },
            ToHost::AppMenu { new_window: false },
            ToHost::AppIcon {
                icon_png: vec![0x89, b'P', b'N', b'G'],
                caption_icon_png: vec![1, 2],
            },
            ToHost::AppIcon {
                icon_png: vec![3],
                caption_icon_png: Vec::new(),
            },
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
            FromHost::Dragging {
                task_id: 7,
                dragging: true,
            },
            FromHost::Shown {
                task_id: 8,
                shown: false,
            },
            FromHost::Caption {
                task_id: 9,
                band: CaptionBand {
                    height: 62,
                    start_width: 80,
                    end_width: 276,
                },
            },
            FromHost::MenuChosen {
                task_id: 10,
                action: MenuAction::NewWindow,
            },
            FromHost::MenuChosen {
                task_id: 11,
                action: MenuAction::AspectRatio,
            },
            FromHost::MenuChosen {
                task_id: 12,
                action: MenuAction::OpenByDefault,
            },
            FromHost::MenuChosen {
                task_id: 13,
                action: MenuAction::AppInfo,
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
                shortcut_id: String::new(),
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

    fn drawn(package: &str, icon: u8, caption: u8) -> AppInfo {
        AppInfo {
            icon_png: vec![icon],
            caption_icon_png: vec![caption],
            ..info(package)
        }
    }

    fn icon_sent(host: HostId, icon: u8, caption: u8) -> HostAction {
        HostAction::Send {
            host,
            message: ToHost::AppIcon {
                icon_png: vec![icon],
                caption_icon_png: vec![caption],
            },
        }
    }

    #[test]
    fn fr7_an_open_window_hears_of_its_apps_new_icon_at_once() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.app(&drawn(CLOCK, 1, 1), now);
        hosts.change(&open(1, CLOCK), now);
        // The program was made with the icon it shows, so nothing is sent with the windows.
        assert_eq!(
            hosts.attached(HostId(1), CLOCK, false, now),
            vec![
                menu(HostId(1), false),
                HostAction::Send {
                    host: HostId(1),
                    message: show(1)
                }
            ]
        );
        // The same icon again changes nothing; a new one reaches the running window host.
        assert!(hosts.app(&drawn(CLOCK, 1, 1), now).is_empty());
        assert_eq!(
            hosts.app(&drawn(CLOCK, 2, 3), now),
            vec![icon_sent(HostId(1), 2, 3)]
        );
        assert!(hosts.app(&drawn(CLOCK, 2, 3), now).is_empty());
        // An icon the guest could not draw leaves the one the windows show.
        assert!(hosts.app(&info(CLOCK), now).is_empty());
        assert!(hosts.app(&drawn(CLOCK, 2, 3), now).is_empty());
    }

    #[test]
    fn fr7_a_window_host_started_without_its_icon_gets_it_when_it_attaches() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        // The guest could not draw the icon yet when the program was made.
        hosts.app(&info(CLOCK), now);
        assert!(matches!(
            hosts.change(&open(1, CLOCK), now).as_slice(),
            [HostAction::Start { .. }]
        ));
        // It could before the window host connected.
        assert!(hosts.app(&drawn(CLOCK, 4, 5), now).is_empty());
        assert_eq!(
            hosts.attached(HostId(1), CLOCK, false, now),
            vec![
                menu(HostId(1), false),
                icon_sent(HostId(1), 4, 5),
                HostAction::Send {
                    host: HostId(1),
                    message: show(1)
                }
            ]
        );
        // Told once.
        assert!(hosts.app(&drawn(CLOCK, 4, 5), now).is_empty());
    }

    #[test]
    fn fr7_a_window_host_started_from_a_shortcut_starts_the_shortcut() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        let shortcut = |id: &str| HostAction::Launch {
            package: CLOCK.into(),
            shortcut: Some(id.into()),
        };
        // Not running yet: the shortcut is started, and its task opens in this window host.
        let first = hosts.attached_for(HostId(1), CLOCK, true, Some("new-alarm"), now);
        assert_eq!(first.last(), Some(&shortcut("new-alarm")));
        // Running already: a second start from another shortcut asks for that shortcut.
        hosts.change(&open(1, CLOCK), now);
        let second = hosts.attached_for(HostId(2), CLOCK, true, Some("timer"), now);
        assert_eq!(second[0], shortcut("timer"));
        // An ID no command line could carry starts the app instead.
        let third = hosts.attached_for(HostId(3), CLOCK, true, Some("bad\nid"), now);
        assert_eq!(
            third[0],
            HostAction::Launch {
                package: CLOCK.into(),
                shortcut: None,
            }
        );
    }

    #[test]
    fn fr7_a_window_host_the_user_started_shows_the_apps_latest_icon() {
        let now = Instant::now();
        let mut hosts = WindowHosts::new();
        hosts.app(&drawn(CLOCK, 6, 7), now);
        assert_eq!(
            hosts.attached(HostId(1), CLOCK, true, now),
            vec![
                menu(HostId(1), false),
                icon_sent(HostId(1), 6, 7),
                HostAction::Launch {
                    package: CLOCK.into(),
                    shortcut: None,
                }
            ]
        );
    }

    #[test]
    fn fr7_an_empty_icon_on_the_wire_is_not_shown() {
        let empty = Body::AppIcon(dsa_protocol::window_host::types::AppIcon {
            icon_png: Vec::new(),
            caption_icon_png: vec![1],
        });
        assert_eq!(to_host_from_wire(&empty), None);
    }

    fn session(playback: Playback, previous: bool, next: bool) -> MediaSession {
        MediaSession {
            package_name: CLOCK.into(),
            app_label: "Clock".into(),
            playback,
            can_play: true,
            can_pause: true,
            can_previous: previous,
            can_next: next,
            ..Default::default()
        }
    }

    fn media_sent(host: HostId, media: Option<MediaButtons>) -> HostAction {
        HostAction::Send {
            host,
            message: ToHost::AppMedia { media },
        }
    }

    #[test]
    fn fr24_the_taskbar_preview_has_media_buttons() {
        let now = Instant::now();
        let mut hosts = named_hosts(now);
        let playing = MediaButtons::of(&session(Playback::Playing, true, false));
        assert_eq!(
            playing,
            MediaButtons {
                playing: true,
                previous: true,
                play: true,
                pause: true,
                next: false,
            }
        );
        // A session that arrives before the app's window host attaches waits for it.
        assert!(hosts.media(CLOCK, Some(playing)).is_empty());
        hosts.change(&open(1, CLOCK), now);
        let attached = hosts.attached(HostId(1), CLOCK, false, now);
        assert_eq!(
            attached,
            vec![
                menu(HostId(1), false),
                media_sent(HostId(1), Some(playing)),
                HostAction::Send {
                    host: HostId(1),
                    message: show(1)
                },
            ]
        );
        // The same state again tells nobody; a change of playback state does.
        assert!(hosts.media(CLOCK, Some(playing)).is_empty());
        let paused = MediaButtons::of(&session(Playback::Paused, true, true));
        assert_eq!(
            hosts.media(CLOCK, Some(paused)),
            vec![media_sent(HostId(1), Some(paused))]
        );
        // A click on the middle button sends what it shows: play while paused.
        assert_eq!(
            hosts.media_pressed(1, MediaButton::Play),
            Some((CLOCK.to_string(), MediaCommand::Play))
        );
        assert_eq!(hosts.media_pressed(1, MediaButton::Pause), None);
        assert_eq!(
            hosts.media_pressed(1, MediaButton::Next),
            Some((CLOCK.to_string(), MediaCommand::Next))
        );
        assert_eq!(
            hosts.media_pressed(1, MediaButton::Previous),
            Some((CLOCK.to_string(), MediaCommand::Previous))
        );
        // A button the session does not allow, or a window that is not known, sends nothing.
        hosts.media(CLOCK, Some(playing));
        assert_eq!(hosts.media_pressed(1, MediaButton::Next), None);
        assert_eq!(
            hosts.media_pressed(1, MediaButton::Pause),
            Some((CLOCK.to_string(), MediaCommand::Pause))
        );
        assert_eq!(hosts.media_pressed(9, MediaButton::Pause), None);
        // The session's end hides the buttons, and a click after it sends nothing.
        assert_eq!(hosts.media(CLOCK, None), vec![media_sent(HostId(1), None)]);
        assert!(hosts.media(CLOCK, None).is_empty());
        assert_eq!(hosts.media_pressed(1, MediaButton::Pause), None);
        // Another app's session never reaches this app's window host.
        assert!(hosts.media(MAPS, Some(playing)).is_empty());
    }

    #[test]
    fn fr24_the_media_buttons_survive_the_wire() {
        let buttons = MediaButtons {
            playing: true,
            previous: false,
            play: true,
            pause: true,
            next: true,
        };
        for message in [
            ToHost::AppMedia {
                media: Some(buttons),
            },
            ToHost::AppMedia { media: None },
        ] {
            assert_eq!(to_host_from_wire(&to_wire(&message)), Some(message));
        }
        for button in [
            MediaButton::Previous,
            MediaButton::Play,
            MediaButton::Pause,
            MediaButton::Next,
        ] {
            let report = FromHost::MediaButton { task_id: 4, button };
            assert_eq!(report_from_wire(&report_to_wire(&report)), Some(report));
        }
        // A button this build does not know is no press.
        let unknown =
            Body::MediaButtonPressed(dsa_protocol::window_host::types::MediaButtonPressed {
                task_id: 4,
                button: 99,
            });
        assert_eq!(report_from_wire(&unknown), None);
    }
}
