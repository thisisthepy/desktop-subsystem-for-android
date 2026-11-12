//! The guest's app tasks as the host knows them.
//!
//! The agent in the guest reports each app task the host shows as a window (`TaskUpdated`,
//! `TaskRemoved`) and the name and icon of each app it has a task of (`AppInfo`), and the
//! hwcomposer reports where each task's frames arrive on the graphics stream
//! (`TaskFrames`). The session keeps the latest of these in a [`TaskTable`]
//! and hands every change on as a [`TaskEvent`], so a compositor that connects late starts
//! from the tasks that exist now rather than from nothing.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::mpsc::Receiver;

use crate::native_abi::NativeLibraries;

/// A rectangle in guest display pixels: left and top inclusive, right and bottom exclusive.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub struct Bounds {
    pub left: i32,
    pub top: i32,
    pub right: i32,
    pub bottom: i32,
}

impl Bounds {
    /// The width in pixels; zero for a rectangle whose right edge is not past its left.
    pub fn width(&self) -> u32 {
        (i64::from(self.right) - i64::from(self.left)).clamp(0, i64::from(u32::MAX)) as u32
    }

    /// The height in pixels; zero for a rectangle whose bottom edge is not below its top.
    pub fn height(&self) -> u32 {
        (i64::from(self.bottom) - i64::from(self.top)).clamp(0, i64::from(u32::MAX)) as u32
    }
}

/// One app task, as the agent last described it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Task {
    /// The guest's task ID, unique while the task lives.
    pub task_id: i32,
    /// Where the task's window is on the guest's freeform display.
    pub bounds: Bounds,
    /// The package of the task's root activity.
    pub package_name: String,
    /// The title the task's window shows.
    pub label: String,
    /// Whether the task has input focus in the guest.
    pub focused: bool,
    /// The density the task's app draws at, in dots per inch (130 at a scale of 100%); 0
    /// when the agent could not read it.
    pub density_dpi: u32,
    /// The largest size the guest can give the task, in its display pixels; 0 when unknown.
    pub max_width: u32,
    pub max_height: u32,
    /// The smallest size the guest gives the task, in its display pixels; 0 when unknown.
    pub min_width: u32,
    pub min_height: u32,
    /// Whether the task's app asked for a light status bar (dark icons) or a dark one.
    pub status_bar: StatusBarAppearance,
    /// The status bar color the app set, as 0xAARRGGBB; 0 when none.
    pub status_bar_color: u32,
    /// The orientation the task's top activity is locked to, if any.
    pub orientation: TaskOrientation,
    /// Whether the task's app asked to fill the screen.
    pub fullscreen: bool,
    /// Whether the task's app went into picture-in-picture.
    pub picture_in_picture: bool,
    /// Whether the task's app draws its own title bar: it asked for a transparent caption bar
    /// background, so the window draws only its caption buttons and the app's icon over it.
    pub transparent_caption: bool,
    /// While [`Task::transparent_caption`] is set: where in the title bar band the app has
    /// controls, relative to the task's top left corner. The rest of the band drags the
    /// window.
    pub caption_interactive: Vec<Bounds>,
    /// Whether the task is past its first screen, so it has somewhere to go back to.
    pub can_go_back: bool,
    /// Whether the task's top activity says it handles a change of density itself, so
    /// Android redraws it at a new density instead of relaunching it.
    pub handles_density_change: bool,
}

/// The orientation a task's top activity asks for.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum TaskOrientation {
    /// Not locked, or unknown.
    #[default]
    Unspecified,
    Portrait,
    Landscape,
}

/// What an app told the system about its status bar's icons.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum StatusBarAppearance {
    /// The app did not say, or the agent could not read it.
    #[default]
    Unspecified,
    /// Dark icons on a light status bar.
    Light,
    /// Light icons on a dark status bar.
    Dark,
}

impl Task {
    /// The largest size the guest can give the task, when the agent said.
    pub fn max_size(&self) -> Option<(u32, u32)> {
        (self.max_width > 0 && self.max_height > 0).then_some((self.max_width, self.max_height))
    }

    /// The smallest size the guest gives the task, when the agent said.
    pub fn min_size(&self) -> Option<(u32, u32)> {
        (self.min_width > 0 || self.min_height > 0).then_some((self.min_width, self.min_height))
    }
}

/// One of Android's settings pages for an app.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash)]
pub enum AppSettingsPage {
    /// The app's info: its permissions, storage and notifications.
    #[default]
    Details,
    /// The app's aspect ratio, which the user may override.
    AspectRatio,
    /// Whether the links the app supports open in the app or in the browser.
    OpenByDefault,
}

/// The name and launcher icon of an app the guest has a task of.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppInfo {
    pub package_name: String,
    /// The app's own name, as a launcher shows it.
    pub label: String,
    /// The app's launcher icon as a PNG image; empty when the guest gave none.
    pub icon_png: Vec<u8>,
    /// The installed version, as Android's package manager reports it. Empty for an app that
    /// only has a task, since a task says nothing of it.
    pub version_name: String,
    pub version_code: i64,
    /// When the app was first installed and last updated, in milliseconds since the Unix
    /// epoch; 0 when unknown.
    pub first_install_time_ms: u64,
    pub last_update_time_ms: u64,
    /// Whether the app is part of the Android image, which cannot be uninstalled.
    pub system_app: bool,
    /// The storage the app takes, in bytes: its code, its data and its cache.
    pub app_bytes: u64,
    pub data_bytes: u64,
    pub cache_bytes: u64,
    /// The ABI the app's native code runs as, such as `x86_64` or `arm64-v8a`; empty for an
    /// app without native code, and for an app that only has a task.
    pub primary_abi: String,
    /// The native libraries the app's APKs on the device hold, base and splits together.
    pub native_libraries: NativeLibraries,
    /// The icon a window's title bar shows: an adaptive icon's foreground layer alone, as a
    /// PNG image; empty for an app whose icon is not adaptive, which shows `icon_png` there.
    pub caption_icon_png: Vec<u8>,
    /// Whether the app supports several windows at once, so its windows offer New window.
    pub multiple_windows: bool,
    /// How far the app may elevate to root, as the service's list of installed apps reports
    /// it: `off`, `ask` or `remember`, or `unavailable` while root elevation is off for the
    /// computer. Empty where nothing reported it, as for an app that only has a task.
    pub root: String,
    /// The app's launcher shortcuts, in the list of installed apps: the static and dynamic
    /// ones in rank order, then the pinned ones. Empty for an app that only has a task.
    pub shortcuts: Vec<AppShortcut>,
    /// The user ID the app shares with other apps (its manifest's `android:sharedUserId`), in
    /// the list of installed apps. Apps with the same value run under one Android uid, so root
    /// elevation treats them as one group and the settings apps show them together. Empty for
    /// an app with a user ID of its own, and for an app that only has a task.
    pub shared_user_id: String,
}

/// One of an app's launcher shortcuts in Android (`ShortcutManager`): a static one from its
/// manifest, a dynamic one it published, or one pinned at its request. The host shows the
/// static and dynamic ones as tasks of the app's jump list, Dock menu or desktop entry, and a
/// pinned one as a desktop shortcut.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct AppShortcut {
    /// Unique within its app.
    pub id: String,
    pub short_label: String,
    /// Empty when the app gave none.
    pub long_label: String,
    /// A square PNG image; empty when the shortcut has none, and the app's icon stands in.
    pub icon_png: Vec<u8>,
    /// Lowest first.
    pub rank: u32,
    pub manifest: bool,
    pub dynamic: bool,
    pub pinned: bool,
}

impl AppShortcut {
    /// Whether the app's jump list (Dock menu, desktop entry actions) shows it: a static or
    /// dynamic shortcut, not one that is only pinned.
    pub fn in_jump_list(&self) -> bool {
        self.manifest || self.dynamic
    }

    /// The name to show: the short label, else the long one, else the ID.
    pub fn label(&self) -> &str {
        [&self.short_label, &self.long_label, &self.id]
            .into_iter()
            .map(|label| label.trim())
            .find(|label| !label.is_empty())
            .unwrap_or("")
    }
}

/// The shortcuts of `shortcuts` an app's jump list shows, in the order it shows them: the
/// static and dynamic ones by rank, at most [`MAX_JUMP_LIST_TASKS`], each ID once.
pub fn jump_list_shortcuts(shortcuts: &[AppShortcut]) -> Vec<&AppShortcut> {
    let mut listed: Vec<&AppShortcut> = shortcuts
        .iter()
        .filter(|shortcut| shortcut.in_jump_list() && is_valid_shortcut_id(&shortcut.id))
        .collect();
    listed.sort_by_key(|shortcut| (!shortcut.manifest, shortcut.rank));
    let mut seen = std::collections::BTreeSet::new();
    listed.retain(|shortcut| seen.insert(shortcut.id.as_str()));
    listed.truncate(MAX_JUMP_LIST_TASKS);
    listed
}

/// The most shortcuts of an app a jump list shows. Android lets an app publish a few more,
/// but a launcher shows about this many, and the taskbar's menu would outgrow the screen.
pub const MAX_JUMP_LIST_TASKS: usize = 10;

/// The most shortcuts the table keeps of one app. Android caps the static and dynamic ones
/// far lower; the rest are pinned ones, and a guest that reports more is broken or hostile.
pub const MAX_SHORTCUTS: usize = 64;

/// The largest shortcut icon the table keeps, in bytes; a larger one is dropped and the
/// shortcut shows the app's icon.
pub const MAX_SHORTCUT_ICON_BYTES: usize = 256 * 1024;

/// Whether `id` can name a shortcut on a command line and in a file: not empty, at most 256
/// characters, and no control characters. Android takes any string as an ID; one that is not
/// this is left out of the jump list rather than passed to a shell.
pub fn is_valid_shortcut_id(id: &str) -> bool {
    !id.is_empty() && id.chars().count() <= 256 && !id.chars().any(char::is_control)
}

/// An app's request to put one of its shortcuts on the desktop (`requestPinShortcut`). The
/// window manager asks the user and answers with [`TaskRequest::AnswerPinShortcut`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PinShortcutRequest {
    pub request_id: u32,
    pub package_name: String,
    /// The app's own name, for the question.
    pub app_label: String,
    pub shortcut: AppShortcut,
}

impl AppInfo {
    /// The storage the app takes in all, in bytes.
    pub fn total_bytes(&self) -> u64 {
        self.app_bytes
            .saturating_add(self.data_bytes)
            .saturating_add(self.cache_bytes)
    }
}

/// A change to the guest's tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskEvent {
    /// A task appeared, or something its window shows changed. It carries the whole task.
    Updated(Task),
    /// The task ended.
    Removed { task_id: i32 },
    /// The task's frames now arrive on the graphics stream as posts to `display_id`.
    Frames { task_id: i32, display_id: u32 },
    /// The name or icon of an app changed, or the app is about to have its first task.
    App(AppInfo),
    /// The cursor of the editor focused in the task moved, or an editor gained the focus.
    /// It only places the input method's windows, so it is passed on as it comes and is not
    /// part of the tasks a late watcher starts from.
    ImeCursor {
        task_id: i32,
        cursor: crate::ime::ImeCursor,
    },
    /// The editor focused in the task changed: its kind, its text or its selection. The
    /// window's input method reads the editor from it, so like the cursor it is passed on as
    /// it comes and is not part of the tasks a late watcher starts from.
    ImeEditor {
        task_id: i32,
        editor: crate::ime::ImeEditor,
    },
    /// The apps installed in the guest that a launcher lists changed. It carries the whole
    /// list, in package order, as the host's start menu shows it.
    Installed(Vec<AppInfo>),
    /// An app asks for one of its shortcuts on the desktop. Like the cursor it is passed on as
    /// it comes and is not part of the tasks a late watcher starts from.
    PinShortcut(PinShortcutRequest),
}

impl TaskEvent {
    /// The task the event is about; `None` for an event about an app.
    pub fn task_id(&self) -> Option<i32> {
        match self {
            TaskEvent::Updated(task) => Some(task.task_id),
            TaskEvent::Removed { task_id }
            | TaskEvent::Frames { task_id, .. }
            | TaskEvent::ImeCursor { task_id, .. }
            | TaskEvent::ImeEditor { task_id, .. } => Some(*task_id),
            TaskEvent::App(_) | TaskEvent::Installed(_) | TaskEvent::PinShortcut(_) => None,
        }
    }
}

/// The guest's tasks as they are now, and every change to them from then on.
pub struct TaskWatch {
    /// The events that bring an empty list of tasks to the current one.
    pub current: Vec<TaskEvent>,
    /// The changes after `current`, in order.
    pub events: Receiver<TaskEvent>,
}

/// What the host asks of the guest about its tasks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRequest {
    /// The user closed the task's window; the agent ends the task.
    Close { task_id: i32 },
    /// Start the app's launcher activity as a new task in its own window.
    Launch { package_name: String },
    /// Start another task of an app that supports several windows, even when it has one, in
    /// a window of its own: the system menu's New window.
    NewWindow { package_name: String },
    /// Open one of Android's settings pages of the app (`page`) in a window of its own, as
    /// the system menu's Aspect ratio and Open by default do.
    OpenAppSettings {
        package_name: String,
        page: AppSettingsPage,
    },
    /// The user resized the task's window: the task takes its client size, in guest
    /// display pixels. `live` is true while the user is still dragging the window's edge:
    /// the task takes the size without being reconfigured, so its activities lay out again
    /// instead of being recreated at every step. The first request with `live` false, sent
    /// when the drag ends, reconfigures the task once for its final size.
    Resize {
        task_id: i32,
        width: u32,
        height: u32,
        live: bool,
    },
    /// The task's window is on a monitor of another scale: the task draws at this density.
    SetDensity { task_id: i32, density_dpi: u32 },
    /// The task's window draws its title bar over the top `height` pixels of the task: the
    /// task gets a top system bar inset of that height. 0 removes it. `start_width` and
    /// `end_width` are how far the window's icon and its caption buttons reach into the band
    /// from its left and right ends, which an app drawing its own title bar keeps clear.
    SetCaption {
        task_id: i32,
        height: u32,
        start_width: u32,
        end_width: u32,
    },
    /// Whether any task window is shown: open and not minimized. Once none has been shown for
    /// a while the guest's screen goes off and the guest goes into Doze; it wakes as soon as
    /// a window is shown again.
    WindowsShown { shown: bool },
    /// End every process of the app and give its memory back to the host, even one kept
    /// running by a widget or a foreground service: the user's Stop.
    StopApp { package_name: String },
    /// Start one of the app's shortcuts as a task in its own window: its jump list task or
    /// desktop shortcut.
    LaunchShortcut {
        package_name: String,
        shortcut_id: String,
    },
    /// The user's answer to an app's request to put a shortcut on the desktop.
    AnswerPinShortcut { request_id: u32, accepted: bool },
}

impl TaskRequest {
    /// What the request asks for, in words for a log line.
    pub fn describe(&self) -> String {
        match self {
            TaskRequest::Close { task_id } => format!("end task {task_id}"),
            TaskRequest::Launch { package_name } => format!("launch {package_name}"),
            TaskRequest::NewWindow { package_name } => {
                format!("open a new window of {package_name}")
            }
            TaskRequest::OpenAppSettings { package_name, page } => {
                let page = match page {
                    AppSettingsPage::Details => "info",
                    AppSettingsPage::AspectRatio => "aspect ratio",
                    AppSettingsPage::OpenByDefault => "open by default",
                };
                format!("open Android's {page} page of {package_name}")
            }
            TaskRequest::Resize {
                task_id,
                width,
                height,
                live: false,
            } => format!("resize task {task_id} to {width}x{height}"),
            TaskRequest::Resize {
                task_id,
                width,
                height,
                live: true,
            } => format!("resize task {task_id} to {width}x{height} while it is dragged"),
            TaskRequest::SetDensity {
                task_id,
                density_dpi,
            } => format!("draw task {task_id} at {density_dpi} dpi"),
            TaskRequest::SetCaption {
                task_id, height, ..
            } => format!("give task {task_id} a title bar inset of {height} pixels"),
            TaskRequest::WindowsShown { shown: true } => "wake for a shown window".into(),
            TaskRequest::WindowsShown { shown: false } => {
                "let the screen go off with no window shown".into()
            }
            TaskRequest::StopApp { package_name } => {
                format!("stop {package_name} and give its memory back")
            }
            TaskRequest::LaunchShortcut {
                package_name,
                shortcut_id,
            } => format!("start the shortcut {shortcut_id:?} of {package_name}"),
            TaskRequest::AnswerPinShortcut {
                request_id,
                accepted: true,
            } => format!("pin the shortcut of request {request_id}"),
            TaskRequest::AnswerPinShortcut {
                request_id,
                accepted: false,
            } => format!("decline the shortcut of request {request_id}"),
        }
    }
}

/// Folds a run of requests waiting to be sent into the ones still worth sending.
///
/// Only a task's latest size and latest density count: while the user drags a window's
/// edge, the sizes it passed through are dropped rather than queued, so the guest never
/// works through a backlog. The latest size goes as it was asked for, live or not, so the
/// request that ends a drag (not live) is never dropped for a live one before it, and a run
/// of steps still being dragged stays live. The latest of each keeps its place, so a density followed by
/// the size that goes with it still arrives in that order. Closes and launches are kept, in
/// order.
pub fn coalesce(requests: Vec<TaskRequest>) -> Vec<TaskRequest> {
    let superseded = |index: usize, request: &TaskRequest| {
        requests[index + 1..]
            .iter()
            .any(|later| match (request, later) {
                (
                    TaskRequest::Resize { task_id: a, .. },
                    TaskRequest::Resize { task_id: b, .. },
                ) => a == b,
                (
                    TaskRequest::SetDensity { task_id: a, .. },
                    TaskRequest::SetDensity { task_id: b, .. },
                ) => a == b,
                (
                    TaskRequest::SetCaption { task_id: a, .. },
                    TaskRequest::SetCaption { task_id: b, .. },
                ) => a == b,
                (TaskRequest::WindowsShown { .. }, TaskRequest::WindowsShown { .. }) => true,
                _ => false,
            })
    };
    requests
        .iter()
        .enumerate()
        .filter(|(index, request)| !superseded(*index, request))
        .map(|(_, request)| request.clone())
        .collect()
}

/// Why a [`TaskRequest`] was not carried out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskRequestError {
    /// The Android is not running, so there is nothing to act on; the detail says so in
    /// words a person can act on.
    NotRunning(String),
    /// The request could not be delivered; the detail says why.
    Failed(String),
}

impl fmt::Display for TaskRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskRequestError::NotRunning(why) | TaskRequestError::Failed(why) => f.write_str(why),
        }
    }
}

impl std::error::Error for TaskRequestError {}

/// The most tasks the table keeps. A guest that reports more is broken or hostile; the
/// tasks past this are dropped rather than allowed to grow the host's memory without end.
pub const MAX_TASKS: usize = 1024;

/// The most apps the table keeps a name and icon for, for the same reason.
pub const MAX_APPS: usize = MAX_TASKS;

/// The largest icon the table keeps, in bytes. A launcher icon of 256 pixels on a side is far
/// smaller; a larger one is dropped and the app keeps its name.
pub const MAX_ICON_BYTES: usize = 1024 * 1024;

/// The most installed apps the table keeps, for the same reason as [`MAX_TASKS`].
pub const MAX_INSTALLED_APPS: usize = 4096;

/// The most bytes of icons the installed apps keep in all. The icons past it are dropped and
/// their apps keep their names, so a guest cannot fill the host's memory with icons.
pub const MAX_INSTALLED_ICON_BYTES: usize = 64 * 1024 * 1024;

/// `app` as the table keeps it: an icon larger than [`MAX_ICON_BYTES`] is dropped, and an
/// empty name is the package's.
fn kept(app: &AppInfo) -> AppInfo {
    let mut app = app.clone();
    if app.icon_png.len() > MAX_ICON_BYTES {
        app.icon_png.clear();
    }
    if app.label.trim().is_empty() {
        app.label = app.package_name.clone();
    }
    app
}

/// `shortcuts` as the table keeps them: at most [`MAX_SHORTCUTS`], each ID once, with no icon
/// past [`MAX_SHORTCUT_ICON_BYTES`] and none without an ID.
fn kept_shortcuts(shortcuts: Vec<AppShortcut>) -> Vec<AppShortcut> {
    let mut seen = std::collections::BTreeSet::new();
    shortcuts
        .into_iter()
        .filter(|shortcut| !shortcut.id.is_empty() && seen.insert(shortcut.id.clone()))
        .take(MAX_SHORTCUTS)
        .map(|mut shortcut| {
            if shortcut.icon_png.len() > MAX_SHORTCUT_ICON_BYTES {
                shortcut.icon_png.clear();
            }
            shortcut
        })
        .collect()
}

/// `apps` in package order, each package once, within [`MAX_INSTALLED_APPS`] and
/// [`MAX_INSTALLED_ICON_BYTES`].
fn installed_list(apps: &[AppInfo]) -> Vec<AppInfo> {
    let mut by_package = BTreeMap::new();
    for app in apps {
        if app.package_name.is_empty() {
            continue;
        }
        if by_package.len() >= MAX_INSTALLED_APPS && !by_package.contains_key(&app.package_name) {
            continue;
        }
        by_package.insert(app.package_name.clone(), kept(app));
    }
    let mut icon_bytes = 0usize;
    by_package
        .into_values()
        .map(|mut app| {
            icon_bytes += app.icon_png.len();
            if icon_bytes > MAX_INSTALLED_ICON_BYTES {
                icon_bytes -= app.icon_png.len();
                app.icon_png.clear();
            }
            app
        })
        .collect()
}

/// The latest state of each task.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TaskTable {
    tasks: BTreeMap<i32, Task>,
    frames: BTreeMap<i32, u32>,
    /// The name and icon of each app, by package. An app is forgotten when its last task
    /// ends; the guest describes it again before its next task.
    apps: BTreeMap<String, AppInfo>,
    /// Where the cursor of each task's focused editor last was, so a cursor that did not
    /// move is not passed on again.
    cursors: BTreeMap<i32, crate::ime::ImeCursor>,
    /// The editor last focused in each task, so a description that did not change is not
    /// passed on again.
    editors: BTreeMap<i32, crate::ime::ImeEditor>,
    /// The apps installed in the guest that a launcher lists, once the guest has said which.
    /// They stay while the guest is stopped, because its apps stay installed.
    installed: Option<Vec<AppInfo>>,
    /// The name and icon of each installed app the guest described, by package, until the
    /// list that names it arrives.
    described: BTreeMap<String, AppInfo>,
    /// The launcher shortcuts of each app, by package, as the guest last gave them. They come
    /// apart from the app's description, and the list of installed apps carries them.
    shortcuts: BTreeMap<String, Vec<AppShortcut>>,
}

impl TaskTable {
    pub fn new() -> TaskTable {
        TaskTable::default()
    }

    /// Records `event`. Returns false when the event changes nothing, or when it would take
    /// the table past [`MAX_TASKS`] and was dropped.
    pub fn apply(&mut self, event: &TaskEvent) -> bool {
        match event {
            TaskEvent::Updated(task) => {
                if self.tasks.get(&task.task_id) == Some(task) {
                    return false;
                }
                if !self.knows(task.task_id) && self.len() >= MAX_TASKS {
                    return false;
                }
                self.tasks.insert(task.task_id, task.clone());
                true
            }
            TaskEvent::Removed { task_id } => {
                let removed = self.tasks.remove(task_id);
                let had_frames = self.frames.remove(task_id).is_some();
                self.cursors.remove(task_id);
                self.editors.remove(task_id);
                if let Some(task) = &removed {
                    let package = &task.package_name;
                    if !self.tasks.values().any(|t| &t.package_name == package) {
                        self.apps.remove(package);
                    }
                }
                removed.is_some() || had_frames
            }
            TaskEvent::App(app) => {
                let mut app = app.clone();
                if app.icon_png.len() > MAX_ICON_BYTES {
                    app.icon_png.clear();
                }
                if self.apps.get(&app.package_name) == Some(&app) {
                    return false;
                }
                if !self.apps.contains_key(&app.package_name) && self.apps.len() >= MAX_APPS {
                    return false;
                }
                self.apps.insert(app.package_name.clone(), app);
                true
            }
            TaskEvent::Frames {
                task_id,
                display_id,
            } => {
                if self.frames.get(task_id) == Some(display_id) {
                    return false;
                }
                if !self.knows(*task_id) && self.len() >= MAX_TASKS {
                    return false;
                }
                self.frames.insert(*task_id, *display_id);
                true
            }
            TaskEvent::ImeCursor { task_id, cursor } => {
                // Only for a task there is a window of.
                if !self.knows(*task_id) || self.cursors.get(task_id) == Some(cursor) {
                    return false;
                }
                self.cursors.insert(*task_id, *cursor);
                true
            }
            TaskEvent::ImeEditor { task_id, editor } => {
                // Only for a task there is a window of.
                if !self.knows(*task_id) || self.editors.get(task_id) == Some(editor) {
                    return false;
                }
                self.editors.insert(*task_id, editor.clone());
                true
            }
            TaskEvent::Installed(apps) => {
                let list = installed_list(apps);
                if self.installed.as_ref() == Some(&list) {
                    return false;
                }
                self.installed = Some(list);
                true
            }
            // Passed on as it comes; nothing of it is kept.
            TaskEvent::PinShortcut(request) => !request.package_name.is_empty(),
        }
    }

    /// Records the shortcuts the guest gave `package`, and returns the list of installed
    /// apps that carries them, when the app is in it and they changed. Past
    /// [`MAX_SHORTCUTS`] the rest are dropped, and so is an icon past
    /// [`MAX_SHORTCUT_ICON_BYTES`].
    pub fn shortcuts_event(
        &mut self,
        package: &str,
        shortcuts: Vec<AppShortcut>,
    ) -> Option<TaskEvent> {
        if package.is_empty() {
            return None;
        }
        let shortcuts = kept_shortcuts(shortcuts);
        if shortcuts.is_empty() {
            self.shortcuts.remove(package);
        } else {
            if !self.shortcuts.contains_key(package) && self.shortcuts.len() >= MAX_INSTALLED_APPS {
                return None;
            }
            self.shortcuts
                .insert(package.to_string(), shortcuts.clone());
        }
        if let Some(app) = self.described.get_mut(package) {
            app.shortcuts = shortcuts.clone();
        }
        let installed = self.installed.as_ref()?;
        let app = installed.iter().find(|app| app.package_name == package)?;
        if app.shortcuts == shortcuts {
            return None;
        }
        let apps: Vec<AppInfo> = installed
            .iter()
            .map(|app| {
                let mut app = app.clone();
                if app.package_name == package {
                    app.shortcuts = shortcuts.clone();
                }
                app
            })
            .collect();
        Some(TaskEvent::Installed(apps))
    }

    /// Records the name and icon the guest gave an installed app, for the list that names it
    /// next ([`TaskTable::installed_event`]). A guest that describes more than
    /// [`MAX_INSTALLED_APPS`] apps without listing them has the rest dropped.
    pub fn describe_installed(&mut self, app: &AppInfo) {
        if app.package_name.is_empty() {
            return;
        }
        if self.described.len() >= MAX_INSTALLED_APPS
            && !self.described.contains_key(&app.package_name)
        {
            return;
        }
        self.described.insert(app.package_name.clone(), kept(app));
    }

    /// The change that makes `packages` the installed apps, each with the name and icon the
    /// guest last gave it. A package the guest never described goes under its own name with no
    /// icon. Apply it to record it.
    pub fn installed_event(&mut self, packages: &[String]) -> TaskEvent {
        let previous: BTreeMap<&str, &AppInfo> = self
            .installed
            .iter()
            .flatten()
            .map(|app| (app.package_name.as_str(), app))
            .collect();
        let apps: Vec<AppInfo> = packages
            .iter()
            .map(|package| {
                self.described
                    .get(package)
                    .or_else(|| previous.get(package.as_str()).copied())
                    .cloned()
                    .unwrap_or_else(|| AppInfo {
                        package_name: package.clone(),
                        label: package.clone(),
                        icon_png: Vec::new(),
                        ..Default::default()
                    })
            })
            .map(|mut app| {
                app.shortcuts = self
                    .shortcuts
                    .get(&app.package_name)
                    .cloned()
                    .unwrap_or_default();
                app
            })
            .collect();
        self.described
            .retain(|package, _| packages.iter().any(|named| named == package));
        // An app that is gone takes its shortcuts with it.
        self.shortcuts
            .retain(|package, _| packages.iter().any(|named| named == package));
        TaskEvent::Installed(installed_list(&apps))
    }

    /// The apps installed in the guest that a launcher lists, in package order; `None` until
    /// the guest has said which.
    pub fn installed(&self) -> Option<Vec<AppInfo>> {
        self.installed.clone()
    }

    /// Whether the table has heard of `task_id`, from the agent or from the hwcomposer.
    pub fn knows(&self, task_id: i32) -> bool {
        self.tasks.contains_key(&task_id) || self.frames.contains_key(&task_id)
    }

    /// How many tasks the table has heard of.
    pub fn len(&self) -> usize {
        self.tasks.len()
            + self
                .frames
                .keys()
                .filter(|task_id| !self.tasks.contains_key(task_id))
                .count()
    }

    pub fn is_empty(&self) -> bool {
        self.tasks.is_empty() && self.frames.is_empty()
    }

    /// The tasks the agent has described, in task ID order.
    pub fn tasks(&self) -> Vec<Task> {
        self.tasks.values().cloned().collect()
    }

    /// Each task's display, in task ID order.
    pub fn frames(&self) -> Vec<(i32, u32)> {
        self.frames
            .iter()
            .map(|(task_id, display_id)| (*task_id, *display_id))
            .collect()
    }

    /// The name and icon of each app the table knows, in package order.
    pub fn apps(&self) -> Vec<AppInfo> {
        self.apps.values().cloned().collect()
    }

    /// The events that bring an empty table to this one: the installed apps when they are
    /// known, then every app, then every task, then every pairing of a task with its display.
    pub fn snapshot(&self) -> Vec<TaskEvent> {
        let installed = self.installed.clone().map(TaskEvent::Installed);
        let apps = installed
            .into_iter()
            .chain(self.apps.values().cloned().map(TaskEvent::App));
        let tasks = self.tasks.values().cloned().map(TaskEvent::Updated);
        let frames = self
            .frames
            .iter()
            .map(|(task_id, display_id)| TaskEvent::Frames {
                task_id: *task_id,
                display_id: *display_id,
            });
        apps.chain(tasks).chain(frames).collect()
    }

    /// Forgets every task, and returns the removals that say so. The installed apps stay:
    /// they are still installed while the guest is stopped.
    pub fn clear(&mut self) -> Vec<TaskEvent> {
        let mut ids: Vec<i32> = self
            .tasks
            .keys()
            .chain(self.frames.keys())
            .copied()
            .collect();
        ids.sort_unstable();
        ids.dedup();
        self.tasks.clear();
        self.frames.clear();
        self.apps.clear();
        self.cursors.clear();
        ids.into_iter()
            .map(|task_id| TaskEvent::Removed { task_id })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(task_id: i32, label: &str) -> Task {
        Task {
            task_id,
            bounds: Bounds {
                left: 0,
                top: 0,
                right: 400,
                bottom: 300,
            },
            package_name: "com.example".into(),
            label: label.into(),
            ..Task::default()
        }
    }

    fn resize(task_id: i32, width: u32) -> TaskRequest {
        TaskRequest::Resize {
            task_id,
            width,
            height: 600,
            live: false,
        }
    }

    fn live_resize(task_id: i32, width: u32) -> TaskRequest {
        TaskRequest::Resize {
            task_id,
            width,
            height: 600,
            live: true,
        }
    }

    #[test]
    fn fr4_the_resize_that_ends_a_drag_is_never_dropped_for_a_live_one() {
        // The drag's steps fold into the request that ends it, which reconfigures the task.
        let requests = vec![live_resize(1, 500), live_resize(1, 520), resize(1, 520)];
        assert_eq!(coalesce(requests), vec![resize(1, 520)]);
        // Steps still being dragged fold into the latest one, which stays live.
        let requests = vec![
            live_resize(1, 500),
            live_resize(2, 300),
            live_resize(1, 540),
        ];
        assert_eq!(
            coalesce(requests),
            vec![live_resize(2, 300), live_resize(1, 540)]
        );
        // A new drag that starts before the last one's end was sent ends with its own.
        let requests = vec![resize(1, 520), live_resize(1, 560), resize(1, 580)];
        assert_eq!(coalesce(requests), vec![resize(1, 580)]);
    }

    #[test]
    fn fr4_a_resize_says_in_words_whether_it_is_live() {
        assert_eq!(resize(1, 500).describe(), "resize task 1 to 500x600");
        assert_eq!(
            live_resize(1, 500).describe(),
            "resize task 1 to 500x600 while it is dragged"
        );
    }

    #[test]
    fn fr4_only_the_latest_size_of_each_task_is_sent() {
        let requests = vec![
            resize(1, 500),
            resize(2, 300),
            resize(1, 520),
            resize(1, 540),
        ];
        assert_eq!(coalesce(requests), vec![resize(2, 300), resize(1, 540)]);
    }

    #[test]
    fn fr4_a_density_and_its_size_keep_their_order() {
        let density = TaskRequest::SetDensity {
            task_id: 1,
            density_dpi: 320,
        };
        let requests = vec![
            resize(1, 500),
            TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 240,
            },
            density.clone(),
            resize(1, 1000),
        ];
        assert_eq!(coalesce(requests), vec![density, resize(1, 1000)]);
    }

    #[test]
    fn fr4_closes_and_launches_are_never_dropped() {
        let launch = TaskRequest::Launch {
            package_name: "com.example".into(),
        };
        let close = TaskRequest::Close { task_id: 1 };
        let requests = vec![close.clone(), launch.clone(), close.clone(), resize(1, 500)];
        assert_eq!(
            coalesce(requests),
            vec![close.clone(), launch, close, resize(1, 500)]
        );
    }

    #[test]
    fn fr22_only_the_latest_caption_height_of_each_task_is_sent() {
        let caption = |task_id, height| TaskRequest::SetCaption {
            task_id,
            height,
            start_width: 0,
            end_width: 0,
        };
        let requests = vec![
            caption(1, 31),
            resize(1, 500),
            caption(2, 47),
            caption(1, 62),
        ];
        assert_eq!(
            coalesce(requests),
            vec![resize(1, 500), caption(2, 47), caption(1, 62)]
        );
        assert_eq!(
            caption(3, 62).describe(),
            "give task 3 a title bar inset of 62 pixels"
        );
    }

    #[test]
    fn fr10_only_the_latest_word_on_shown_windows_is_sent() {
        let hidden = TaskRequest::WindowsShown { shown: false };
        let shown = TaskRequest::WindowsShown { shown: true };
        let requests = vec![
            hidden.clone(),
            resize(1, 500),
            shown.clone(),
            hidden.clone(),
        ];
        assert_eq!(coalesce(requests), vec![resize(1, 500), hidden]);
        assert_eq!(coalesce(vec![shown.clone()]), vec![shown]);
    }

    #[test]
    fn fr4_a_task_reports_its_size_limit_only_when_it_knows_it() {
        let mut known = task(1, "a");
        assert_eq!(known.max_size(), None);
        known.max_width = 1920;
        assert_eq!(known.max_size(), None);
        known.max_height = 1080;
        assert_eq!(known.max_size(), Some((1920, 1080)));
    }

    #[test]
    fn fr3_the_table_keeps_only_the_latest_state_of_each_task() {
        let mut table = TaskTable::new();
        assert!(table.apply(&TaskEvent::Updated(task(7, "Clock"))));
        assert!(table.apply(&TaskEvent::Updated(task(7, "Alarm"))));
        assert!(!table.apply(&TaskEvent::Updated(task(7, "Alarm"))));
        assert!(table.apply(&TaskEvent::Frames {
            task_id: 7,
            display_id: 3
        }));
        assert_eq!(
            table.snapshot(),
            vec![
                TaskEvent::Updated(task(7, "Alarm")),
                TaskEvent::Frames {
                    task_id: 7,
                    display_id: 3
                }
            ]
        );
        assert!(table.apply(&TaskEvent::Removed { task_id: 7 }));
        assert!(table.is_empty());
        assert!(!table.apply(&TaskEvent::Removed { task_id: 7 }));
    }

    #[test]
    fn fr3_frames_may_arrive_before_the_task_they_belong_to() {
        let mut table = TaskTable::new();
        table.apply(&TaskEvent::Frames {
            task_id: 9,
            display_id: 4,
        });
        assert!(table.knows(9));
        assert_eq!(table.len(), 1);
        table.apply(&TaskEvent::Updated(task(9, "Maps")));
        assert_eq!(table.len(), 1);
        assert_eq!(table.frames(), vec![(9, 4)]);
    }

    #[test]
    fn fr3_clearing_the_table_removes_every_task_it_knew() {
        let mut table = TaskTable::new();
        table.apply(&TaskEvent::Updated(task(2, "a")));
        table.apply(&TaskEvent::Frames {
            task_id: 2,
            display_id: 1,
        });
        table.apply(&TaskEvent::Frames {
            task_id: 5,
            display_id: 2,
        });
        assert_eq!(
            table.clear(),
            vec![
                TaskEvent::Removed { task_id: 2 },
                TaskEvent::Removed { task_id: 5 }
            ]
        );
        assert!(table.is_empty());
    }

    #[test]
    fn nfr5_a_guest_cannot_grow_the_table_without_end() {
        let mut table = TaskTable::new();
        for task_id in 0..MAX_TASKS as i32 {
            assert!(table.apply(&TaskEvent::Updated(task(task_id, "t"))));
        }
        assert!(!table.apply(&TaskEvent::Updated(task(-1, "one too many"))));
        assert!(!table.apply(&TaskEvent::Frames {
            task_id: -1,
            display_id: 1
        }));
        // A task it already knows still changes.
        assert!(table.apply(&TaskEvent::Updated(task(0, "renamed"))));
        assert_eq!(table.len(), MAX_TASKS);
    }

    fn app(package: &str, label: &str) -> AppInfo {
        AppInfo {
            package_name: package.into(),
            label: label.into(),
            icon_png: vec![0x89, b'P', b'N', b'G'],
            ..Default::default()
        }
    }

    #[test]
    fn fr3_an_apps_name_and_icon_come_before_its_tasks_and_go_with_the_last_one() {
        let mut table = TaskTable::new();
        assert!(table.apply(&TaskEvent::App(app("com.example", "Example"))));
        assert!(!table.apply(&TaskEvent::App(app("com.example", "Example"))));
        table.apply(&TaskEvent::Updated(task(1, "a")));
        table.apply(&TaskEvent::Updated(task(2, "b")));
        assert_eq!(
            table.snapshot()[0],
            TaskEvent::App(app("com.example", "Example"))
        );
        table.apply(&TaskEvent::Removed { task_id: 1 });
        assert_eq!(table.apps(), vec![app("com.example", "Example")]);
        table.apply(&TaskEvent::Removed { task_id: 2 });
        assert!(table.apps().is_empty());
    }

    #[test]
    fn nfr5_a_guest_cannot_grow_the_apps_without_end() {
        let mut table = TaskTable::new();
        let mut huge = app("com.huge", "Huge");
        huge.icon_png = vec![0; MAX_ICON_BYTES + 1];
        assert!(table.apply(&TaskEvent::App(huge)));
        assert_eq!(table.apps()[0].label, "Huge");
        assert!(table.apps()[0].icon_png.is_empty());
        for n in 1..MAX_APPS {
            assert!(table.apply(&TaskEvent::App(app(&format!("p{n}"), "p"))));
        }
        assert!(!table.apply(&TaskEvent::App(app("one.too.many", "x"))));
        assert!(table.apply(&TaskEvent::App(app("p1", "renamed"))));
    }

    #[test]
    fn fr7_the_installed_apps_carry_the_names_and_icons_the_guest_gave() {
        let mut table = TaskTable::new();
        assert_eq!(table.installed(), None, "unknown until the guest says");
        table.describe_installed(&app("com.example.clock", "Clock"));
        table.describe_installed(&app("com.example.maps", "Maps"));
        let event = table.installed_event(&["com.example.maps".into(), "com.example.clock".into()]);
        assert!(table.apply(&event));
        assert!(!table.apply(&event), "the same list is no news");
        assert_eq!(
            table.installed(),
            Some(vec![
                app("com.example.clock", "Clock"),
                app("com.example.maps", "Maps")
            ])
        );
        assert_eq!(
            table.snapshot(),
            vec![TaskEvent::Installed(vec![
                app("com.example.clock", "Clock"),
                app("com.example.maps", "Maps")
            ])]
        );

        // Maps is removed; Clock keeps what it was given without being described again.
        let event = table.installed_event(&["com.example.clock".into()]);
        assert!(table.apply(&event));
        assert_eq!(
            table.installed(),
            Some(vec![app("com.example.clock", "Clock")])
        );

        // An app the guest never described goes under its package's name.
        let event = table.installed_event(&["org.example.bare".into()]);
        table.apply(&event);
        let bare = &table.installed().unwrap()[0];
        assert_eq!(bare.label, "org.example.bare");
        assert!(bare.icon_png.is_empty());
    }

    #[test]
    fn fr7_the_installed_apps_stay_while_the_guest_is_stopped() {
        let mut table = TaskTable::new();
        table.describe_installed(&app("com.example.clock", "Clock"));
        let event = table.installed_event(&["com.example.clock".into()]);
        table.apply(&event);
        table.apply(&TaskEvent::Updated(task(1, "a")));
        table.clear();
        assert_eq!(
            table.installed(),
            Some(vec![app("com.example.clock", "Clock")])
        );
        assert_eq!(table.installed_event(&[]), TaskEvent::Installed(Vec::new()));
    }

    #[test]
    fn nfr5_a_guest_cannot_grow_the_installed_apps_without_end() {
        let mut table = TaskTable::new();
        let mut huge = app("com.huge", "Huge");
        huge.icon_png = vec![0; MAX_ICON_BYTES + 1];
        table.describe_installed(&huge);
        let names: Vec<String> = (0..MAX_INSTALLED_APPS + 10)
            .map(|n| format!("p{n:05}"))
            .chain(std::iter::once("com.huge".to_string()))
            .collect();
        let event = table.installed_event(&names);
        table.apply(&event);
        let installed = table.installed().unwrap();
        assert_eq!(installed.len(), MAX_INSTALLED_APPS);
        assert!(installed
            .iter()
            .find(|app| app.package_name == "com.huge")
            .is_none_or(|app| app.icon_png.is_empty()));
        let total: usize = installed.iter().map(|app| app.icon_png.len()).sum();
        assert!(total <= MAX_INSTALLED_ICON_BYTES);
    }

    #[test]
    fn fr3_bounds_never_give_a_negative_size() {
        let bounds = Bounds {
            left: 10,
            top: 10,
            right: 5,
            bottom: 30,
        };
        assert_eq!(bounds.width(), 0);
        assert_eq!(bounds.height(), 20);
    }

    #[test]
    fn fr6_a_cursor_is_passed_on_when_it_moves_and_only_for_a_known_task() {
        let mut table = TaskTable::new();
        let cursor = |left| TaskEvent::ImeCursor {
            task_id: 7,
            cursor: crate::ime::ImeCursor {
                left,
                top: 10,
                right: left + 2,
                bottom: 40,
            },
        };
        assert!(!table.apply(&cursor(5)), "no such task");
        table.apply(&TaskEvent::Updated(task(7, "Notes")));
        assert!(table.apply(&cursor(5)));
        assert!(!table.apply(&cursor(5)), "it did not move");
        assert!(table.apply(&cursor(6)));
        // A late watcher starts from the tasks alone.
        assert_eq!(table.snapshot(), vec![TaskEvent::Updated(task(7, "Notes"))]);
        table.apply(&TaskEvent::Removed { task_id: 7 });
        table.apply(&TaskEvent::Updated(task(7, "Notes")));
        assert!(table.apply(&cursor(6)), "a new task's cursor is news");
    }

    #[test]
    fn fr6_an_editor_description_is_passed_on_when_it_changes_and_only_for_a_known_task() {
        let mut table = TaskTable::new();
        let editor = |text: &str| TaskEvent::ImeEditor {
            task_id: 7,
            editor: crate::ime::ImeEditor {
                kind: crate::ime::InputKind::Text,
                text: text.into(),
                text_start: 0,
                selection: (1, 1),
                composition: None,
            },
        };
        assert!(!table.apply(&editor("a")), "no such task");
        table.apply(&TaskEvent::Updated(task(7, "Notes")));
        assert!(table.apply(&editor("a")));
        assert!(!table.apply(&editor("a")), "nothing changed");
        assert!(table.apply(&editor("ab")));
        // A late watcher starts from the tasks alone.
        assert_eq!(table.snapshot(), vec![TaskEvent::Updated(task(7, "Notes"))]);
        table.apply(&TaskEvent::Removed { task_id: 7 });
        table.apply(&TaskEvent::Updated(task(7, "Notes")));
        assert!(table.apply(&editor("ab")), "a new task's editor is news");
    }

    fn chat_shortcut(id: &str, rank: u32, manifest: bool, dynamic: bool) -> AppShortcut {
        AppShortcut {
            id: id.into(),
            short_label: id.to_uppercase(),
            rank,
            manifest,
            dynamic,
            ..AppShortcut::default()
        }
    }

    fn list_chat(table: &mut TaskTable) {
        table.describe_installed(&AppInfo {
            package_name: "com.example.chat".into(),
            label: "Chat".into(),
            ..AppInfo::default()
        });
        let event = table.installed_event(&["com.example.chat".into()]);
        table.apply(&event);
    }

    #[test]
    fn fr7_shortcuts_ride_with_the_installed_app_and_survive_its_next_description() {
        let mut table = TaskTable::new();
        list_chat(&mut table);
        let event = table
            .shortcuts_event("com.example.chat", vec![chat_shortcut("a", 0, false, true)])
            .expect("the list changes");
        assert!(table.apply(&event));
        assert_eq!(table.installed().unwrap()[0].shortcuts.len(), 1);
        // The same shortcuts again change nothing.
        assert_eq!(
            table.shortcuts_event("com.example.chat", vec![chat_shortcut("a", 0, false, true)]),
            None
        );
        // An update describes the app again without its shortcuts; the list keeps them.
        list_chat(&mut table);
        assert_eq!(table.installed().unwrap()[0].shortcuts[0].id, "a");
        // Shortcuts of an app that is not listed wait for it; an uninstall drops them.
        assert_eq!(
            table.shortcuts_event("com.example.new", vec![chat_shortcut("b", 0, true, false)]),
            None
        );
        let gone = table.installed_event(&[]);
        table.apply(&gone);
        list_chat(&mut table);
        assert!(table.installed().unwrap()[0].shortcuts.is_empty());
    }

    #[test]
    fn fr7_a_guest_cannot_flood_the_host_with_shortcuts() {
        let mut table = TaskTable::new();
        list_chat(&mut table);
        let mut many: Vec<AppShortcut> = (0..200)
            .map(|n| chat_shortcut(&format!("s{n}"), n, false, true))
            .collect();
        many[0].icon_png = vec![0; MAX_SHORTCUT_ICON_BYTES + 1];
        many.push(chat_shortcut("s1", 1, false, true));
        many.push(chat_shortcut("", 1, false, true));
        let event = table.shortcuts_event("com.example.chat", many).unwrap();
        table.apply(&event);
        let kept = &table.installed().unwrap()[0].shortcuts;
        assert_eq!(kept.len(), MAX_SHORTCUTS);
        assert!(kept[0].icon_png.is_empty());
    }

    #[test]
    fn fr7_the_jump_list_shows_static_then_dynamic_shortcuts_by_rank_and_no_pinned_ones() {
        let pinned = AppShortcut {
            pinned: true,
            ..chat_shortcut("pinned", 0, false, false)
        };
        let shortcuts = vec![
            chat_shortcut("dynamic-1", 1, false, true),
            pinned,
            chat_shortcut("dynamic-0", 0, false, true),
            chat_shortcut("static", 5, true, false),
            chat_shortcut("bad\u{7}id", 0, true, false),
        ];
        let listed: Vec<&str> = jump_list_shortcuts(&shortcuts)
            .into_iter()
            .map(|shortcut| shortcut.id.as_str())
            .collect();
        assert_eq!(listed, vec!["static", "dynamic-0", "dynamic-1"]);
        let many: Vec<AppShortcut> = (0..30)
            .map(|n| chat_shortcut(&format!("s{n}"), n, false, true))
            .collect();
        assert_eq!(jump_list_shortcuts(&many).len(), MAX_JUMP_LIST_TASKS);
    }

    #[test]
    fn fr7_a_shortcut_shows_its_short_label_else_its_long_label_else_its_id() {
        let mut shortcut = chat_shortcut("id", 0, true, false);
        assert_eq!(shortcut.label(), "ID");
        shortcut.short_label = " ".into();
        shortcut.long_label = "Long".into();
        assert_eq!(shortcut.label(), "Long");
        shortcut.long_label.clear();
        assert_eq!(shortcut.label(), "id");
    }
}
