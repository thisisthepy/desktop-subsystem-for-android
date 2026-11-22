//! Which window each guest task has, and which window each display's frames go to.
//!
//! The compositor shows one host window per guest app task, and nothing else. This module
//! holds the rules; the platform's compositor carries out the [`WindowChange`]s it returns
//! and asks it where a frame belongs. A frame goes to a window only when the hwcomposer has
//! said which task posts to that display (`TaskFrames`) and that task has a window. Every
//! other frame is dropped, so no window ever shows the guest's whole screen.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use super::caption::{reported_tone, CaptionBand, CaptionTone};
use super::window_mode::WindowMode;
use super::window_sizing::{
    density_for_dpi, scale_between, to_task_length, to_window_rect, to_window_size, Scale,
    ScalingMode, WindowSizing, BASE_DENSITY, DENSITY_AT_BASE_DPI, SETTLE,
};
use crate::app_windows::{
    below_title_bar_packages, full_screen_place, AppWindowSettings, FullScreenDefaults,
    FullScreenPlace,
};
use crate::tasks::{Bounds, Task, TaskEvent, TaskOrientation, TaskRequest};

/// The display the guest's whole freeform screen is posted to. Its frames are never shown,
/// whatever the guest says about it.
pub const WHOLE_SCREEN_DISPLAY: u32 = 0;

/// The size a window opens at when the guest reports a task without a size.
pub const DEFAULT_WINDOW_SIZE: (u32, u32) = (480, 800);

/// What the platform's windows must do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WindowChange {
    /// Open a window for the task, with a client area of `width` x `height` pixels.
    Open {
        task_id: i32,
        /// The package of the task's app, whose window host opens the window.
        package_name: String,
        title: String,
        width: u32,
        height: u32,
        /// Whether the task has input focus in the guest, so the window opens active.
        focused: bool,
    },
    /// The title of the task's window changed.
    Retitle { task_id: i32, title: String },
    /// The window's client area takes this size: the guest resized the task on its own, or
    /// did not take the size the window asked for.
    Resize {
        task_id: i32,
        width: u32,
        height: u32,
    },
    /// Close the task's window.
    Close { task_id: i32 },
    /// The task's size in the guest, which the window measures its pointer input in, and the
    /// largest and smallest client areas the guest gives it. It changes with every size the
    /// window asks for, without the window itself changing. `scale` is how large the window
    /// shows each of the task's pixels (window pixels per task pixels): 1:1, except for a task
    /// that keeps its density on a monitor of another scale, whose window shows its frames
    /// and measures its pointer input at that scale. Comes again after every `Resize`.
    TaskSize {
        task_id: i32,
        width: u32,
        height: u32,
        max: Option<(u32, u32)>,
        min: Option<(u32, u32)>,
        scale: Scale,
    },
    /// The cursor of the editor focused in the task is here, in the window's client pixels:
    /// the window puts its input method's windows beside it. Comes again when the window
    /// shows its task at a new scale.
    ImeCursor {
        task_id: i32,
        cursor: crate::ime::ImeCursor,
    },
    /// The editor focused in the task is this: the window's input method reads the editor's
    /// kind and its text around the selection from it.
    ImeEditor {
        task_id: i32,
        editor: crate::ime::ImeEditor,
    },
    /// The tone the app asked its status bar's icons to take, which the window's caption
    /// buttons take, or `None` when the app expressed nothing and the window reads the tone
    /// from the pixels under its band ([`super::caption::shown_tone`]). Comes with every
    /// window that opens, and whenever the app's status bar changes tone.
    CaptionTone {
        task_id: i32,
        tone: Option<CaptionTone>,
    },
    /// How the window is shown (as the app asks), and whether the user may resize it (as the
    /// user's settings for the app say). Comes with a window that opens unless it is an
    /// ordinary resizable one, and with every change.
    Mode {
        task_id: i32,
        mode: WindowMode,
        resizable: bool,
    },
    /// Whether the app draws its own title bar, and where its controls are in the band, in
    /// the window's client pixels (the task's at the window's scale). Comes with a window that
    /// opens for an app that draws its own, with every change, and with a new scale.
    ///
    /// `below` says the task's app starts below the title bar, as the user chose for the app:
    /// the window paints its band itself, opaque, in the color the app's frame has there
    /// ([`super::caption::below_title_bar_sample`]). Such a window is never `app_drawn`.
    CaptionMode {
        task_id: i32,
        app_drawn: bool,
        interactive: Vec<Bounds>,
        below: bool,
    },
    /// Whether the task has somewhere to go back to, so the title bar shows a back arrow in
    /// the place of the app's icon. Comes with a window that opens past its first screen, and
    /// with every change.
    CaptionBack { task_id: i32, back: bool },
    /// The window opened for task `task_id` shows task `shown_task_id` from now on: a task
    /// that took the window over after a GPU reset (see [`super::gpu_reset`]). The window keeps
    /// its ID; what it sends Android itself goes to the task it shows. The task rules never make
    /// it; it comes from [`super::gpu_reset::GpuResetWindows`], by window ID.
    ShownTask { task_id: i32, shown_task_id: i32 },
    /// Where the app has the overlay the window shows (the top left corner of its surface on
    /// the guest's display, and the display's size), and whether the overlay may take the
    /// keyboard focus and takes pointer input. Comes with a window that opens for an app
    /// overlay window, after its [`WindowChange::Mode`], and with every change, as when the
    /// app moves the overlay. The window host places the window from it
    /// ([`super::overlay_windows`]).
    Overlay {
        task_id: i32,
        left: i32,
        top: i32,
        display: (u32, u32),
        focusable: bool,
        touchable: bool,
    },
}

/// An app overlay window the guest reported, and its window.
#[derive(Debug, Clone)]
struct Overlay {
    /// The overlay as the guest last described it.
    task: Task,
    /// Whether it has a window: not one that covers the guest's whole display.
    window: bool,
    /// The DPI of the monitor its window reported; 0 until it has.
    dpi: u32,
}

impl Overlay {
    /// How large the window shows each of the overlay's pixels: the overlay is drawn at the
    /// guest display's density, and shown the size it would be at its monitor's.
    fn scale(&self) -> Scale {
        if self.dpi == 0 || self.task.density_dpi == 0 {
            return (1, 1);
        }
        scale_between(density_for_dpi(self.dpi), self.task.density_dpi)
    }

    /// The window's client size: the overlay's size at its scale.
    fn window_size(&self) -> (u32, u32) {
        to_window_size(window_size(&self.task.bounds), self.scale())
    }

    /// The change that tells the window where the overlay is and what it takes.
    fn place(&self) -> WindowChange {
        let overlay = self.task.overlay.unwrap_or_default();
        WindowChange::Overlay {
            task_id: self.task.task_id,
            left: self.task.bounds.left,
            top: self.task.bounds.top,
            display: (self.task.max_width, self.task.max_height),
            focusable: overlay.focusable,
            touchable: overlay.touchable,
        }
    }

    /// The changes that give the window its size and its scale.
    fn size_changes(&self) -> Vec<WindowChange> {
        let (width, height) = self.window_size();
        let (task_width, task_height) = window_size(&self.task.bounds);
        vec![
            WindowChange::Resize {
                task_id: self.task.task_id,
                width,
                height,
            },
            WindowChange::TaskSize {
                task_id: self.task.task_id,
                width: task_width,
                height: task_height,
                max: None,
                min: None,
                scale: self.scale(),
            },
        ]
    }

    /// Everything a window that opens for the overlay must do.
    fn open_changes(&self) -> Vec<WindowChange> {
        let (width, height) = self.window_size();
        let mut changes = vec![
            WindowChange::Open {
                task_id: self.task.task_id,
                package_name: self.task.package_name.clone(),
                title: window_title(&self.task),
                width,
                height,
                // An overlay never opens active: it takes the focus only by the user's click.
                focused: false,
            },
            WindowChange::Mode {
                task_id: self.task.task_id,
                mode: WindowMode::Overlay,
                resizable: false,
            },
            self.place(),
        ];
        if self.scale() != (1, 1) {
            changes.extend(self.size_changes());
        }
        changes
    }
}

/// The title of a task's window: its label, else its package, else its ID.
pub fn window_title(task: &Task) -> String {
    if !task.label.trim().is_empty() {
        task.label.clone()
    } else if !task.package_name.is_empty() {
        task.package_name.clone()
    } else {
        format!("Task {}", task.task_id)
    }
}

/// The client area size of a task's window, in pixels: the task's size on the guest's
/// display, or [`DEFAULT_WINDOW_SIZE`] when the guest gave none.
pub fn window_size(bounds: &Bounds) -> (u32, u32) {
    match (bounds.width(), bounds.height()) {
        (0, _) | (_, 0) => DEFAULT_WINDOW_SIZE,
        size => size,
    }
}

/// The largest size of the same shape as `size` that fits in `area`, or `size` itself when it
/// fits already. A task taller than the screen opens smaller rather than off the screen, and
/// its frames keep their proportions.
pub fn fit_within(size: (u32, u32), area: (u32, u32)) -> (u32, u32) {
    let (width, height) = size;
    let (max_width, max_height) = (area.0.max(1), area.1.max(1));
    if width <= max_width && height <= max_height {
        return size;
    }
    let scale = f64::min(
        f64::from(max_width) / f64::from(width.max(1)),
        f64::from(max_height) / f64::from(height.max(1)),
    );
    let fit = |length: u32, max: u32| ((f64::from(length) * scale).round() as u32).clamp(1, max);
    (fit(width, max_width), fit(height, max_height))
}

/// The tone `task`'s app asked for the caption glyphs of its window; `None` when it
/// expressed nothing.
fn tone_of(task: &Task) -> Option<CaptionTone> {
    reported_tone(task.status_bar, task.status_bar_color)
}

/// Whether `task`'s app draws its own title bar, where its controls are in the band, and
/// whether the app starts below the title bar. The controls count only while it draws its
/// own, and an app that starts below the title bar never does: the user's switch wins over
/// the app's request, and the whole band is the window's.
fn caption_mode_of(task: &Task) -> (bool, Vec<Bounds>, bool) {
    if task.below_title_bar {
        (false, Vec::new(), true)
    } else if task.transparent_caption {
        (true, task.caption_interactive.clone(), false)
    } else {
        (false, Vec::new(), false)
    }
}

/// The change that tells `task`'s window whether its app draws its own title bar, with the
/// app's controls in the window's pixels at `scale`.
fn caption_mode_change(task: &Task, scale: Scale) -> WindowChange {
    let (app_drawn, interactive, below) = caption_mode_of(task);
    WindowChange::CaptionMode {
        task_id: task.task_id,
        app_drawn,
        interactive: interactive
            .into_iter()
            .map(|rect| to_window_rect(rect, scale))
            .collect(),
        below,
    }
}

/// What `task`'s window, which shows it at `scale`, must change in its title bar since
/// `previous`, or, for a window that opens (`previous` is `None`), what differs from a
/// window's own title bar showing the icon.
fn caption_changes(previous: Option<&Task>, task: &Task, scale: Scale) -> Vec<WindowChange> {
    let mut changes = Vec::new();
    let mode = caption_mode_of(task);
    let was = previous.map_or((false, Vec::new(), false), caption_mode_of);
    if mode != was {
        changes.push(caption_mode_change(task, scale));
    }
    if previous.is_some_and(|previous| previous.can_go_back) != task.can_go_back {
        changes.push(WindowChange::CaptionBack {
            task_id: task.task_id,
            back: task.can_go_back,
        });
    }
    changes
}

fn open(task: &Task) -> WindowChange {
    let (width, height) = window_size(&task.bounds);
    WindowChange::Open {
        task_id: task.task_id,
        package_name: task.package_name.clone(),
        title: window_title(task),
        width,
        height,
        focused: task.focused,
    }
}

/// A task's size in the guest, and the largest client area its window may have.
use super::window_sizing::TaskSize;

/// What a window that opened on a monitor does: the client size it takes, the requests for
/// its task, and what else it must hear.
pub type WindowOpened = ((u32, u32), Vec<TaskRequest>, Vec<WindowChange>);

/// The size, in the guest's pixels, that `task` is first given before its window opens, when
/// it is to open at another size than the guest gave it: the start size the user chose for
/// the app, else the task's size turned on its side when its app is locked to an orientation
/// the size does not have (a portrait app opens taller than it is wide). Kept within what the
/// guest can draw. `None` when the window opens at the task's size.
pub fn start_size(task: &Task, app: Option<&AppWindowSettings>) -> Option<(u32, u32)> {
    let current = window_size(&task.bounds);
    let density = if task.density_dpi == 0 {
        DENSITY_AT_BASE_DPI
    } else {
        task.density_dpi
    };
    let wanted = match app.and_then(|app| app.start_size_dp) {
        Some((width, height)) => {
            let px = |dp: u32| {
                (u64::from(dp) * u64::from(density) / u64::from(BASE_DENSITY)).max(1) as u32
            };
            (px(width), px(height))
        }
        None => {
            let (width, height) = current;
            match task.orientation {
                TaskOrientation::Portrait if width > height => (height, width),
                TaskOrientation::Landscape if height > width => (height, width),
                _ => return None,
            }
        }
    };
    let wanted = match task.max_size() {
        Some(max) => fit_within(wanted, max),
        None => wanted,
    };
    (wanted != current).then_some(wanted)
}

/// A task whose window waits for the guest to give it its start size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PendingOpen {
    size: (u32, u32),
    /// When the guest was asked for it; `None` until it is.
    asked: Option<Instant>,
}

/// The compositor's tasks and their windows.
#[derive(Debug, Default)]
pub struct TaskWindows {
    /// The tasks the agent has described.
    tasks: BTreeMap<i32, Task>,
    /// The display each task's frames arrive on. It may be known before the task is.
    displays: BTreeMap<i32, u32>,
    /// Tasks whose window the user closed, while the guest ends them.
    closing: BTreeSet<i32>,
    /// How each task's window and the task follow each other's size and scale.
    sizes: BTreeMap<i32, WindowSizing>,
    /// The task size, limits and scale each window was last told.
    told_sizes: BTreeMap<i32, (TaskSize, Scale)>,
    /// The user's window settings for each app, by package.
    app_windows: BTreeMap<String, AppWindowSettings>,
    /// Whether the user's window settings were read yet. Until then nothing is known of which
    /// apps start below the title bar, and the guest is told nothing.
    app_windows_known: bool,
    /// The apps the guest was last told start below the title bar; `None` when it must be
    /// told again.
    told_below_title_bar: Option<Vec<String>>,
    /// Where every app without its own choice shows its full screen.
    fullscreen_defaults: FullScreenDefaults,
    /// Whether each task was last told to keep a clear full screen request inside its window.
    told_full_screen: BTreeMap<i32, bool>,
    /// Tasks whose window opens once the guest has given them their start size.
    pending: BTreeMap<i32, PendingOpen>,
    /// The mode and resizability each window was last told.
    told_modes: BTreeMap<i32, (WindowMode, bool)>,
    /// Where the guest last said the cursor of each task's focused editor is, in the task's
    /// pixels: shown again at a window's new scale, since the window has it in its own.
    cursors: BTreeMap<i32, crate::ime::ImeCursor>,
    /// The title bar band each window last reported, in its pixels, with the scale its
    /// caption inset was given to the task at: given again at a window's new scale, since the
    /// task has it in its own pixels. With them, whether the band was last given as none,
    /// because the task's app asks to fill the screen.
    bands: BTreeMap<i32, (CaptionBand, Scale, bool)>,
    /// What the window manager logs, until it takes it ([`TaskWindows::take_notes`]).
    notes: Vec<String>,
    /// The app overlay windows the guest reported. They are kept apart from the tasks: none
    /// of what the host asks of a task (a size, a density, a title bar, an end) is ever asked
    /// of an overlay, whose app alone moves, sizes and removes it.
    overlays: BTreeMap<i32, Overlay>,
}

/// `size` as `WxH`, or "none" when there is no limit.
fn describe_size(size: Option<(u32, u32)>) -> String {
    size.map_or_else(
        || "none".to_string(),
        |(width, height)| format!("{width}x{height}"),
    )
}

/// `cursor`, in a task's pixels, where its window shows it at `scale`.
fn shown_cursor(cursor: crate::ime::ImeCursor, scale: Scale) -> crate::ime::ImeCursor {
    let shown = to_window_rect(
        Bounds {
            left: cursor.left,
            top: cursor.top,
            right: cursor.right,
            bottom: cursor.bottom,
        },
        scale,
    );
    crate::ime::ImeCursor {
        left: shown.left,
        top: shown.top,
        right: shown.right,
        bottom: shown.bottom,
    }
}

/// The band a task is given: none while its app asks to fill the screen (`cleared`).
fn given_band(band: CaptionBand, cleared: bool) -> CaptionBand {
    if cleared {
        CaptionBand::default()
    } else {
        band
    }
}

/// The caption inset of a task whose window draws `band` (window pixels) and shows the task
/// at `scale`: the same band in the task's pixels.
fn caption_request(task_id: i32, band: CaptionBand, scale: Scale) -> TaskRequest {
    TaskRequest::SetCaption {
        task_id,
        height: to_task_length(band.height, scale),
        start_width: to_task_length(band.start_width, scale),
        end_width: to_task_length(band.end_width, scale),
    }
}

impl TaskWindows {
    pub fn new() -> TaskWindows {
        TaskWindows::default()
    }

    /// The lines the window manager logs about what the tasks did, once each: a size the
    /// guest gave a task in place of the one its window asked for.
    pub fn take_notes(&mut self) -> Vec<String> {
        std::mem::take(&mut self.notes)
    }

    /// Whether the task has a window now.
    pub fn has_window(&self, task_id: i32) -> bool {
        self.has_overlay_window(task_id)
            || (self.tasks.contains_key(&task_id)
                && !self.closing.contains(&task_id)
                && !self.pending.contains_key(&task_id))
    }

    /// Whether `task_id` is an app overlay window that has a window.
    pub fn has_overlay_window(&self, task_id: i32) -> bool {
        self.overlays
            .get(&task_id)
            .is_some_and(|overlay| overlay.window)
    }

    /// Whether `task_id` is an app overlay window, with a window or not.
    pub fn is_overlay(&self, task_id: i32) -> bool {
        self.overlays.contains_key(&task_id)
    }

    /// The guest described an app overlay window: it opens a window of its own, follows where
    /// its app has it and what size its app gives it, and closes when it comes to cover the
    /// guest's whole display.
    fn overlay_updated(&mut self, task: &Task) -> Vec<WindowChange> {
        let task_id = task.task_id;
        let shows = !task.covers_display();
        let previous = self.overlays.get(&task_id).cloned();
        let had_window = previous.as_ref().is_some_and(|overlay| overlay.window);
        let now = Overlay {
            task: task.clone(),
            window: shows,
            dpi: previous
                .as_ref()
                .filter(|_| had_window && shows)
                .map_or(0, |overlay| overlay.dpi),
        };
        self.overlays.insert(task_id, now.clone());
        match (previous.filter(|_| had_window), shows) {
            (None, false) => Vec::new(),
            (Some(_), false) => {
                // It covers the display now: the guest draws it over the apps' windows.
                self.displays.remove(&task_id);
                vec![WindowChange::Close { task_id }]
            }
            (None, true) => now.open_changes(),
            (Some(previous), true) => {
                let mut changes = Vec::new();
                let title = window_title(task);
                if window_title(&previous.task) != title {
                    changes.push(WindowChange::Retitle { task_id, title });
                }
                if previous.window_size() != now.window_size()
                    || previous.scale() != now.scale()
                    || window_size(&previous.task.bounds) != window_size(&task.bounds)
                {
                    changes.extend(now.size_changes());
                }
                if previous.place() != now.place() {
                    changes.push(now.place());
                }
                changes
            }
        }
    }

    /// The window of the overlay `task_id` is on a monitor of `dpi`: it shows the overlay the
    /// size its app meant it to be there. `None` when `task_id` is no overlay with a window.
    fn overlay_dpi(&mut self, task_id: i32, dpi: u32) -> Option<Vec<WindowChange>> {
        let overlay = self.overlays.get_mut(&task_id).filter(|o| o.window)?;
        let before = (overlay.window_size(), overlay.scale());
        overlay.dpi = dpi;
        if before == (overlay.window_size(), overlay.scale()) {
            return Some(Vec::new());
        }
        Some(overlay.size_changes())
    }

    /// The package of the task's root activity, while the task is known.
    pub fn package_of(&self, task_id: i32) -> Option<&str> {
        self.tasks
            .get(&task_id)
            .map(|task| task.package_name.as_str())
    }

    /// The packages of the tasks that have a window, each once, in task ID order of their
    /// first window: the apps to open again once Android has started again.
    pub fn window_packages(&self) -> Vec<String> {
        let mut packages: Vec<String> = Vec::new();
        for task_id in self.windows() {
            if let Some(package) = self.package_of(task_id) {
                if !packages.iter().any(|known| known == package) {
                    packages.push(package.to_string());
                }
            }
        }
        packages
    }

    /// The tasks that have a window, in task ID order, then the app overlay windows that have
    /// one: an overlay that is shown keeps Android awake as a task's window does.
    pub fn windows(&self) -> Vec<i32> {
        self.tasks
            .keys()
            .copied()
            .filter(|task_id| {
                !self.closing.contains(task_id) && !self.pending.contains_key(task_id)
            })
            .chain(
                self.overlays
                    .iter()
                    .filter(|(_, overlay)| overlay.window)
                    .map(|(task_id, _)| *task_id),
            )
            .collect()
    }

    /// Takes the user's window settings for every app, which the windows that open from now
    /// on follow. A window open already keeps its size, and its resizability follows at once.
    pub fn set_app_windows(&mut self, apps: &[AppWindowSettings]) -> Vec<WindowChange> {
        self.app_windows = apps
            .iter()
            .map(|app| (app.package_name.clone(), app.clone()))
            .collect();
        self.app_windows_known = true;
        self.retell_modes()
    }

    /// Takes where every app without its own choice shows its full screen, which the windows
    /// open already follow at once.
    pub fn set_full_screen_defaults(&mut self, defaults: FullScreenDefaults) -> Vec<WindowChange> {
        self.fullscreen_defaults = defaults.resolved();
        self.retell_modes()
    }

    /// The mode changes of every open window after a change of the user's settings.
    fn retell_modes(&mut self) -> Vec<WindowChange> {
        let open = self.windows();
        open.into_iter()
            .filter_map(|task_id| self.mode_change(task_id, false))
            .collect()
    }

    /// Where `task`'s window shows its app's full screen: `None` while it asks for none.
    fn full_screen_place_of(&self, task: &Task) -> Option<FullScreenPlace> {
        full_screen_place(
            task.fullscreen_request,
            self.app_windows.get(&task.package_name),
            self.fullscreen_defaults,
        )
    }

    /// How `task`'s window follows a monitor of another scale: as the user chose for its app,
    /// else as DSA's built-in list says for it, and as its top activity says it handles a
    /// change of density.
    fn scaling_of(&self, task: &Task) -> ScalingMode {
        self.app_windows
            .get(&task.package_name)
            .map(|app| app.dpi_scaling)
            .unwrap_or_default()
            .mode_for(&task.package_name, task.handles_density_change)
    }

    /// A new record of how `task`'s window follows its task.
    fn sizing_for(&self, task: &Task) -> WindowSizing {
        WindowSizing::with_mode(task, self.scaling_of(task))
    }

    /// The scale `task_id`'s window shows its task at.
    fn scale_of(&self, task_id: i32) -> Scale {
        if let Some(overlay) = self.overlays.get(&task_id) {
            return overlay.scale();
        }
        self.sizes.get(&task_id).map_or((1, 1), WindowSizing::scale)
    }

    /// How large `task_id`'s window shows each of its task's pixels: window pixels per task
    /// pixels.
    pub fn scale(&self, task_id: i32) -> Scale {
        self.scale_of(task_id)
    }

    /// The mode and resizability `task`'s window has.
    fn mode_of(&self, task: &Task) -> (WindowMode, bool) {
        let fixed = self
            .app_windows
            .get(&task.package_name)
            .is_some_and(|app| app.fixed_size);
        (
            WindowMode::of(
                self.full_screen_place_of(task) == Some(FullScreenPlace::Screen),
                task.picture_in_picture,
            ),
            !fixed,
        )
    }

    /// The change that tells the task's window its mode, when it differs from what it was
    /// last told; a window that has just opened is taken to be an ordinary resizable one.
    fn mode_change(&mut self, task_id: i32, opened: bool) -> Option<WindowChange> {
        let task = self.tasks.get(&task_id)?;
        let now = self.mode_of(task);
        if opened {
            self.told_modes.remove(&task_id);
        }
        let told = self
            .told_modes
            .get(&task_id)
            .copied()
            .unwrap_or((WindowMode::Normal, true));
        self.told_modes.insert(task_id, now);
        (told != now).then_some(WindowChange::Mode {
            task_id,
            mode: now.0,
            resizable: now.1,
        })
    }

    /// What a window that opens for `task` must do: open, take its caption tone, its mode and
    /// its task's size and limits.
    fn open_window(&mut self, task_id: i32) -> Vec<WindowChange> {
        let Some(task) = self.tasks.get(&task_id).cloned() else {
            return Vec::new();
        };
        let sizing = self.sizing_for(&task);
        self.sizes.insert(task_id, sizing);
        self.told_sizes.remove(&task_id);
        let mut changes = vec![
            open(&task),
            WindowChange::CaptionTone {
                task_id,
                tone: tone_of(&task),
            },
        ];
        changes.extend(caption_changes(None, &task, self.scale_of(task_id)));
        changes.extend(self.mode_change(task_id, true));
        changes.extend(self.task_size_change(task_id));
        changes
    }

    /// Records a change to the guest's tasks, and returns what the windows must do.
    pub fn apply(&mut self, event: &TaskEvent) -> Vec<WindowChange> {
        // A task stays the app's whose window it opened in: an older guest may name another
        // package for it (a permission dialog that became the task's root), and taking that
        // would move the window to another app's program or close the app's.
        if let TaskEvent::Updated(task) = event {
            if let Some(owner) = self
                .tasks
                .get(&task.task_id)
                .map(|known| known.package_name.clone())
                .filter(|owner| !owner.is_empty() && *owner != task.package_name)
            {
                self.notes.push(format!(
                    "task {} was reported as {}'s, but its window is {}'s; it stays {}'s",
                    task.task_id, task.package_name, owner, owner
                ));
                let mut kept = task.clone();
                kept.package_name = owner;
                return self.apply(&TaskEvent::Updated(kept));
            }
        }
        match event {
            TaskEvent::Updated(task) if task.is_overlay() => self.overlay_updated(task),
            TaskEvent::Removed { task_id } if self.overlays.contains_key(task_id) => {
                self.displays.remove(task_id);
                self.cursors.remove(task_id);
                match self.overlays.remove(task_id) {
                    Some(overlay) if overlay.window => {
                        vec![WindowChange::Close { task_id: *task_id }]
                    }
                    _ => Vec::new(),
                }
            }
            // An app's name and icon belong to its program, not to a window, and the installed
            // apps to the start menu.
            TaskEvent::App(_) | TaskEvent::Installed(_) => Vec::new(),
            TaskEvent::Updated(task) if self.pending.contains_key(&task.task_id) => {
                self.tasks.insert(task.task_id, task.clone());
                if self.closing.contains(&task.task_id) {
                    return Vec::new();
                }
                let wanted = self.pending[&task.task_id].size;
                if window_size(&task.bounds) != wanted {
                    return Vec::new();
                }
                // The guest gave the task its start size: the window opens at it, with the
                // frames the app draws at it.
                self.pending.remove(&task.task_id);
                self.open_window(task.task_id)
            }
            TaskEvent::Updated(task) if !self.tasks.contains_key(&task.task_id) => {
                self.tasks.insert(task.task_id, task.clone());
                if !super::window_hosts::is_valid_package(&task.package_name) {
                    // No window program can be made for it, so its window would never open:
                    // said once, when the task first comes.
                    self.notes.push(format!(
                        "task {} ({:?}, {:?}) gets no window: {:?} is not a package name a \
                         window program can be made for",
                        task.task_id,
                        task.package_name,
                        window_title(task),
                        task.package_name,
                    ));
                }
                if self.closing.contains(&task.task_id) {
                    let sizing = self.sizing_for(task);
                    self.sizes.insert(task.task_id, sizing);
                    return Vec::new();
                }
                match start_size(task, self.app_windows.get(&task.package_name)) {
                    Some(size) => {
                        self.pending
                            .insert(task.task_id, PendingOpen { size, asked: None });
                        Vec::new()
                    }
                    None => self.open_window(task.task_id),
                }
            }
            TaskEvent::Updated(task) => {
                let previous = self.tasks.insert(task.task_id, task.clone());
                let scaling = self.scaling_of(task);
                let follow = match self.sizes.get_mut(&task.task_id) {
                    Some(sizing) if previous.is_some() => {
                        // Another activity on top may say otherwise about a change of density.
                        sizing.set_mode(scaling);
                        let follow = sizing.guest_updated(task);
                        if let Some((asked, given)) = sizing.take_clamped() {
                            self.notes.push(format!(
                                "task {} took {}x{}, not the {}x{} its window asked for: \
                                 Android keeps it within its smallest size ({}) and its largest \
                                 ({}); the window takes the task's size",
                                task.task_id,
                                given.0,
                                given.1,
                                asked.0,
                                asked.1,
                                describe_size(task.min_size()),
                                describe_size(task.max_size()),
                            ));
                        }
                        if let Some((late, kept)) = sizing.take_stale() {
                            self.notes.push(format!(
                                "task {} reported {}x{}, a size its window was dragged past, \
                                 after it had reported {}x{}: the window keeps the size the \
                                 user let go at and asks Android for it again",
                                task.task_id, late.0, late.1, kept.0, kept.1,
                            ));
                        }
                        follow
                    }
                    _ => {
                        let sizing = self.sizing_for(task);
                        self.sizes.insert(task.task_id, sizing);
                        None
                    }
                };
                if self.closing.contains(&task.task_id) {
                    return Vec::new();
                }
                let scale = self.scale_of(task.task_id);
                let mut changes = match previous {
                    None => {
                        let mut changes = vec![
                            open(task),
                            WindowChange::CaptionTone {
                                task_id: task.task_id,
                                tone: tone_of(task),
                            },
                        ];
                        changes.extend(caption_changes(None, task, scale));
                        changes
                    }
                    Some(previous) => {
                        let mut changes = Vec::new();
                        if tone_of(&previous) != tone_of(task) {
                            changes.push(WindowChange::CaptionTone {
                                task_id: task.task_id,
                                tone: tone_of(task),
                            });
                        }
                        changes.extend(caption_changes(Some(&previous), task, scale));
                        let title = window_title(task);
                        if window_title(&previous) != title {
                            changes.push(WindowChange::Retitle {
                                task_id: task.task_id,
                                title,
                            });
                        }
                        if let Some((width, height)) = follow {
                            changes.push(WindowChange::Resize {
                                task_id: task.task_id,
                                width,
                                height,
                            });
                            self.retell_size(task.task_id);
                        }
                        changes.extend(self.mode_change(task.task_id, false));
                        changes
                    }
                };
                changes.extend(self.task_size_change(task.task_id));
                changes
            }
            TaskEvent::Removed { task_id } => {
                self.displays.remove(task_id);
                self.sizes.remove(task_id);
                self.told_sizes.remove(task_id);
                self.told_modes.remove(task_id);
                self.cursors.remove(task_id);
                self.bands.remove(task_id);
                self.told_full_screen.remove(task_id);
                if self.pending.remove(task_id).is_some() {
                    self.closing.remove(task_id);
                    self.tasks.remove(task_id);
                    return Vec::new();
                }
                let was_closing = self.closing.remove(task_id);
                if self.tasks.remove(task_id).is_some() && !was_closing {
                    vec![WindowChange::Close { task_id: *task_id }]
                } else {
                    Vec::new()
                }
            }
            TaskEvent::Frames {
                task_id,
                display_id,
            } => {
                if *display_id == WHOLE_SCREEN_DISPLAY {
                    // Showing it would put the guest's whole screen in a task's window.
                    if self.displays.remove(task_id).is_some() || !self.tasks.contains_key(task_id)
                    {
                        self.notes.push(format!(
                            "task {task_id}'s frames on display {display_id} are not shown: that \
                             display is the guest's whole screen"
                        ));
                    }
                    return Vec::new();
                }
                // A display carries one task's frames: the task that claimed it last.
                self.displays
                    .retain(|other, display| *display != *display_id || other == task_id);
                self.displays.insert(*task_id, *display_id);
                Vec::new()
            }
            // The task's pixels are its window's, so the cursor passes through as it is, unless
            // the window shows its task scaled.
            TaskEvent::ImeCursor { task_id, cursor } => {
                if !self.has_window(*task_id) {
                    return Vec::new();
                }
                self.cursors.insert(*task_id, *cursor);
                vec![WindowChange::ImeCursor {
                    task_id: *task_id,
                    cursor: shown_cursor(*cursor, self.scale_of(*task_id)),
                }]
            }
            TaskEvent::ImeEditor { task_id, editor } => {
                if !self.has_window(*task_id) {
                    return Vec::new();
                }
                vec![WindowChange::ImeEditor {
                    task_id: *task_id,
                    editor: editor.clone(),
                }]
            }
            // The window manager asks the user about it apart from the windows.
            TaskEvent::PinShortcut(_) => Vec::new(),
        }
    }

    /// The task whose window shows the frames posted to `display_id`, if any. Frames for
    /// any other display are dropped.
    pub fn task_for_display(&self, display_id: u32) -> Option<i32> {
        if display_id == WHOLE_SCREEN_DISPLAY {
            return None;
        }
        self.displays
            .iter()
            .find(|(_, display)| **display == display_id)
            .map(|(task_id, _)| *task_id)
            .filter(|task_id| self.has_window(*task_id))
    }

    /// Each display whose frames a window shows, with the task that window belongs to, in
    /// display order.
    pub fn routes(&self) -> Vec<(u32, i32)> {
        let mut routes: Vec<(u32, i32)> = self
            .displays
            .iter()
            .filter(|(task_id, display)| {
                **display != WHOLE_SCREEN_DISPLAY && self.has_window(**task_id)
            })
            .map(|(task_id, display)| (*display, *task_id))
            .collect();
        routes.sort_unstable();
        routes
    }

    /// The user closed the task's window. Returns the request that asks the guest to end
    /// the task, and from now on the task has no window, even if the guest describes it
    /// again before it ends. `None` when the task has no window.
    pub fn user_closed(&mut self, task_id: i32) -> Option<TaskRequest> {
        // An overlay has no close button, and the host never ends one: its app does.
        if !self.has_window(task_id) || self.is_overlay(task_id) {
            return None;
        }
        self.closing.insert(task_id);
        Some(TaskRequest::Close { task_id })
    }

    /// The task's window went away without the user closing it (its window program crashed),
    /// and the task goes on: its window opens again, as it first opened, with everything the
    /// new window has to be told. Nothing for a task that has no window.
    pub fn reopen(&mut self, task_id: i32) -> Vec<WindowChange> {
        if let Some(overlay) = self.overlays.get_mut(&task_id).filter(|o| o.window) {
            // The new window says which monitor it is on.
            overlay.dpi = 0;
            return overlay.open_changes();
        }
        if !self.has_window(task_id) {
            return Vec::new();
        }
        self.open_window(task_id)
    }

    /// The request to end a task the user closed did not reach the guest, so the task is
    /// still there: its window opens again.
    pub fn close_failed(&mut self, task_id: i32) -> Vec<WindowChange> {
        if !self.closing.remove(&task_id) {
            return Vec::new();
        }
        if !self.tasks.contains_key(&task_id) {
            return Vec::new();
        }
        self.open_window(task_id)
    }

    /// The changes that tell the task's window its task size, limits and scale, when they
    /// differ from what it was last told. A new window was shown at the task's size, 1:1, so
    /// only a limit is news to it. A new scale also places the controls of an app that draws
    /// its own title bar again.
    fn task_size_change(&mut self, task_id: i32) -> Vec<WindowChange> {
        let Some(sizing) = self.sizes.get(&task_id) else {
            return Vec::new();
        };
        let now = (sizing.task_size(), sizing.scale());
        let told = match self.told_sizes.get(&task_id) {
            Some(told) => *told,
            None => {
                let Some(shown) = self
                    .tasks
                    .get(&task_id)
                    .map(|task| window_size(&task.bounds))
                else {
                    return Vec::new();
                };
                ((shown, None, None), (1, 1))
            }
        };
        if told == now {
            return Vec::new();
        }
        self.told_sizes.insert(task_id, now);
        let (((width, height), max, min), scale) = now;
        let mut changes = vec![WindowChange::TaskSize {
            task_id,
            width,
            height,
            max,
            min,
            scale,
        }];
        if told.1 != scale {
            if let Some(task) = self.tasks.get(&task_id).filter(|t| t.transparent_caption) {
                changes.push(caption_mode_change(task, scale));
            }
            // The editor's cursor is where the window now shows it, so the input method's
            // windows stay beside it.
            if let Some(cursor) = self.cursors.get(&task_id) {
                changes.push(WindowChange::ImeCursor {
                    task_id,
                    cursor: shown_cursor(*cursor, scale),
                });
            }
        }
        changes
    }

    /// The task's window is to hear its task's size again with the next change, as after a
    /// `Resize`, which a window takes as its task's size too. That holds only at 1:1.
    fn retell_size(&mut self, task_id: i32) {
        if let Some(told) = self.told_sizes.get_mut(&task_id) {
            if told.1 != (1, 1) {
                // No window was ever told a size of nothing, so the next one differs.
                told.0 .0 = (0, 0);
            }
        }
    }

    /// The task's window reports the scale of its monitor: the first report is the monitor
    /// it opened on, every later one a move. Returns what the window must do (take a size
    /// that keeps the app the same size to the eye, when it opened on a monitor of another
    /// scale) and the density request.
    ///
    /// A move asks for no density at once: the window shows its task scaled and hears the
    /// scale. A task that follows its monitor is asked for the new density when the move ends
    /// ([`Self::window_dragging`], or [`Self::reconfigures`] for a move outside a drag). A task
    /// whose app does not say it handles a change of density is never asked for another
    /// unless the user chose the app mode for it (Android then recreates its activity).
    pub fn window_dpi(&mut self, task_id: i32, dpi: u32) -> (Vec<WindowChange>, Vec<TaskRequest>) {
        // An overlay is never asked for a density: its window shows it at its monitor's scale.
        if let Some(changes) = self.overlay_dpi(task_id, dpi) {
            return (changes, Vec::new());
        }
        if !self.has_window(task_id) {
            return (Vec::new(), Vec::new());
        }
        let Some(sizing) = self.sizes.get_mut(&task_id) else {
            return (Vec::new(), Vec::new());
        };
        let (size, request) = sizing.dpi_reported(dpi);
        let mut changes: Vec<WindowChange> = size
            .map(|(width, height)| WindowChange::Resize {
                task_id,
                width,
                height,
            })
            .into_iter()
            .collect();
        if !changes.is_empty() {
            self.retell_size(task_id);
        }
        changes.extend(self.task_size_change(task_id));
        let mut requests: Vec<TaskRequest> = request.into_iter().collect();
        requests.extend(self.recaption(task_id));
        (changes, requests)
    }

    /// The task's window draws its title bar over its top `band.height` pixels, with its icon
    /// and its caption buttons reaching `band.start_width` and `band.end_width` in from the
    /// ends. Returns the request that gives the task a top inset of that height, and tells an
    /// app that draws its own title bar where to keep clear. The band is in the window's
    /// pixels and the inset in the task's, so a window that shows its task at another scale
    /// later gives it the inset again ([`Self::reconfigures`]).
    pub fn window_caption(&mut self, task_id: i32, band: CaptionBand) -> Vec<TaskRequest> {
        // An overlay's window has no title bar, and an overlay takes no inset.
        if !self.has_window(task_id) || self.is_overlay(task_id) {
            return Vec::new();
        }
        let scale = self.scale_of(task_id);
        let cleared = self.band_cleared(task_id);
        self.bands.insert(task_id, (band, scale, cleared));
        vec![caption_request(task_id, given_band(band, cleared), scale)]
    }

    /// The task's window opened on a monitor of `dpi`. Returns the client size it takes, the
    /// requests that have the task draw at that monitor's density, and what else the window
    /// must hear: the scale it shows a task at that keeps its density. `None` when the task
    /// has no window.
    pub fn window_opened(&mut self, task_id: i32, dpi: u32) -> Option<WindowOpened> {
        if let Some(changes) = self.overlay_dpi(task_id, dpi) {
            let size = self.overlays.get(&task_id)?.window_size();
            // The size is returned; the window still has to hear its scale.
            let changes = changes
                .into_iter()
                .filter(|change| !matches!(change, WindowChange::Resize { .. }))
                .collect();
            return Some((size, Vec::new(), changes));
        }
        if !self.has_window(task_id) {
            return None;
        }
        let (size, request) = self.sizes.get_mut(&task_id)?.opened(dpi);
        // The window takes the size as it would a Resize.
        self.retell_size(task_id);
        let changes = self.task_size_change(task_id);
        let mut requests: Vec<TaskRequest> = request.into_iter().collect();
        requests.extend(self.recaption(task_id));
        Some((size, requests, changes))
    }

    /// The task's window has a client area of `view` pixels now. Returns the request that
    /// gives the task that size.
    pub fn window_resized(
        &mut self,
        task_id: i32,
        view: (u32, u32),
        now: Instant,
    ) -> Vec<TaskRequest> {
        if !self.has_window(task_id) {
            return Vec::new();
        }
        let Some(sizing) = self.sizes.get_mut(&task_id) else {
            return Vec::new();
        };
        let request = sizing.view_resized(view, now);
        if let Some(late) = sizing.take_late_echo() {
            self.notes.push(format!(
                "task {task_id}: its window reported {}x{}, a size the drag passed, after the \
                 drag ended at another; Android is not asked for it, and keeps the size the \
                 user let go at",
                late.0, late.1,
            ));
        }
        request.into_iter().collect()
    }

    /// The task's window is on a monitor of `dpi` now. Returns what the window must hear (the
    /// scale it shows its task at until the task draws at that monitor's density, or for good
    /// for a task that keeps its density). The density itself is asked for when the move ends
    /// ([`Self::window_dragging`], [`Self::reconfigures`]).
    pub fn window_dpi_changed(
        &mut self,
        task_id: i32,
        dpi: u32,
    ) -> (Vec<WindowChange>, Vec<TaskRequest>) {
        if let Some(changes) = self.overlay_dpi(task_id, dpi) {
            return (changes, Vec::new());
        }
        if !self.has_window(task_id) {
            return (Vec::new(), Vec::new());
        }
        let mut requests: Vec<TaskRequest> = self
            .sizes
            .get_mut(&task_id)
            .and_then(|sizing| sizing.dpi_changed(dpi))
            .into_iter()
            .collect();
        let changes = self.task_size_change(task_id);
        requests.extend(self.recaption(task_id));
        (changes, requests)
    }

    /// The user started or finished dragging the task's window. The sizes the window asks
    /// for while it is dragged are live; when a drag that resized the task ends, returns the
    /// request that reconfigures the task for its final size.
    ///
    /// A drag that took the window onto a monitor of another scale showed its task scaled, at
    /// the density it kept. Its end, the end of the move, asks for the monitor's density and
    /// the window's client size at it, 1:1, once however many monitors the drag crossed: the
    /// window keeps its size in pixels. A task that keeps its density is asked for neither.
    pub fn window_dragging(
        &mut self,
        task_id: i32,
        active: bool,
        now: Instant,
    ) -> Vec<TaskRequest> {
        if !self.has_window(task_id) {
            return Vec::new();
        }
        let Some(sizing) = self.sizes.get_mut(&task_id) else {
            return Vec::new();
        };
        let mut requests: Vec<TaskRequest> = sizing.dragging(active, now).into_iter().collect();
        // A drag that took the window onto a monitor of another scale ends the move.
        requests.extend(sizing.settle(now));
        requests
    }

    /// The task's size as the guest last reported it, which its frames and its pointer
    /// input are measured in.
    pub fn task_size(&self, task_id: i32) -> Option<(u32, u32)> {
        Some(self.sizes.get(&task_id)?.guest_size())
    }

    /// The largest client area the task's window may have, when the guest said, at the
    /// window's scale.
    pub fn max_client_size(&self, task_id: i32) -> Option<(u32, u32)> {
        self.sizes.get(&task_id)?.max_client_size()
    }

    /// Time has passed: the windows whose task did not take the size they asked for take the
    /// task's size instead.
    pub fn tick(&mut self, now: Instant) -> Vec<WindowChange> {
        let mut changes = Vec::new();
        // A guest that did not give a task its start size in time: its window opens at the
        // size the task has, rather than not at all.
        let late: Vec<i32> = self
            .pending
            .iter()
            .filter(|(_, pending)| {
                pending
                    .asked
                    .is_some_and(|asked| now.saturating_duration_since(asked) >= SETTLE)
            })
            .map(|(task_id, _)| *task_id)
            .collect();
        for task_id in late {
            self.pending.remove(&task_id);
            if !self.closing.contains(&task_id) {
                changes.extend(self.open_window(task_id));
            }
        }
        let mut resized = Vec::new();
        let mut rescaled = Vec::new();
        for (task_id, sizing) in &mut self.sizes {
            if self.closing.contains(task_id) || !self.tasks.contains_key(task_id) {
                continue;
            }
            let scale = sizing.scale();
            let tick = sizing.tick(now);
            if sizing.scale() != scale {
                // A window that waited for its task's new density in vain shows it 1:1.
                rescaled.push(*task_id);
            }
            if let Some((width, height)) = tick {
                changes.push(WindowChange::Resize {
                    task_id: *task_id,
                    width,
                    height,
                });
                resized.push(*task_id);
            }
        }
        for task_id in resized {
            self.retell_size(task_id);
            changes.extend(self.task_size_change(task_id));
        }
        for task_id in rescaled {
            changes.extend(self.task_size_change(task_id));
        }
        changes
    }

    /// The requests that reconfigure each task for a size its window gave it live outside a
    /// drag (a maximise, a snap), now that the task has taken it, and that give the caption
    /// inset again, in the task's pixels, to a task whose window shows it at another scale
    /// than when its band was last given. Asked after every change to the tasks and at every
    /// tick.
    pub fn reconfigures(&mut self, now: Instant) -> Vec<TaskRequest> {
        let banded: Vec<i32> = self.bands.keys().copied().collect();
        let mut requests: Vec<TaskRequest> = self.caption_compat().into_iter().collect();
        requests.extend(
            banded
                .into_iter()
                .filter_map(|task_id| self.recaption(task_id)),
        );
        requests.extend(self.full_screen_requests());
        requests.extend(self.size_reconfigures(now));
        requests
    }

    /// The request that tells the guest which apps start below the title bar, when the user's
    /// settings say another list than the guest was last told. The guest settles a task's
    /// insets when the task starts, so it hears the list before any window asks for it; an
    /// empty list is told too, since the guest may hold an older one.
    fn caption_compat(&mut self) -> Option<TaskRequest> {
        if !self.app_windows_known {
            return None;
        }
        let apps: Vec<AppWindowSettings> = self.app_windows.values().cloned().collect();
        let packages = below_title_bar_packages(&apps);
        if self.told_below_title_bar.as_ref() == Some(&packages) {
            return None;
        }
        self.told_below_title_bar = Some(packages.clone());
        Some(TaskRequest::CaptionCompat {
            below_title_bar: packages,
        })
    }

    /// The caption inset again, when `task_id`'s window shows its task at another scale than
    /// the one its band was last given at.
    fn recaption(&mut self, task_id: i32) -> Option<TaskRequest> {
        if !self.has_window(task_id) {
            return None;
        }
        let scale = self.scale_of(task_id);
        let cleared = self.band_cleared(task_id);
        let (band, given, given_cleared) = self.bands.get_mut(&task_id)?;
        if *given == scale && *given_cleared == cleared {
            return None;
        }
        *given = scale;
        *given_cleared = cleared;
        Some(caption_request(task_id, given_band(*band, cleared), scale))
    }

    /// The requests that tell each task with a window where a clear full screen request of its
    /// app shows, when that differs from what it was last told: the guest keeps a task that
    /// leaves freeform for fullscreen inside its window, at the size it had, only when the
    /// user's setting says so.
    fn full_screen_requests(&mut self) -> Vec<TaskRequest> {
        let mut requests = Vec::new();
        for task_id in self.windows() {
            let Some(task) = self.tasks.get(&task_id) else {
                continue;
            };
            let in_window = full_screen_place(
                crate::tasks::FullScreenRequest::Explicit,
                self.app_windows.get(&task.package_name),
                self.fullscreen_defaults,
            ) == Some(FullScreenPlace::Window);
            // The guest lets every task fill the screen until it is told otherwise.
            let told = self
                .told_full_screen
                .get(&task_id)
                .copied()
                .unwrap_or(false);
            if told != in_window {
                self.told_full_screen.insert(task_id, in_window);
                requests.push(TaskRequest::SetFullScreen { task_id, in_window });
            }
        }
        requests
    }

    /// Whether `task_id` is given no title band: its app asks to fill the screen. Its window
    /// either fills the monitor with no title bar, or keeps its title bar over an app drawn to
    /// the window's edges, as the user's setting for that kind of request says.
    fn band_cleared(&self, task_id: i32) -> bool {
        self.tasks
            .get(&task_id)
            .is_some_and(Task::asks_for_full_screen)
    }

    fn size_reconfigures(&mut self, now: Instant) -> Vec<TaskRequest> {
        let mut requests = Vec::new();
        for (task_id, pending) in &mut self.pending {
            if pending.asked.is_none() && !self.closing.contains(task_id) {
                pending.asked = Some(now);
                requests.push(TaskRequest::Resize {
                    task_id: *task_id,
                    width: pending.size.0,
                    height: pending.size.1,
                    live: false,
                });
            }
        }
        for (task_id, sizing) in &mut self.sizes {
            if self.closing.contains(task_id) || !self.tasks.contains_key(task_id) {
                continue;
            }
            // A move outside a drag has ended: the task takes its monitor's density at the
            // window's size, after the window has taken the size the desktop gave it.
            requests.extend(sizing.settle(now));
            requests.extend(sizing.reconfigure(now));
        }
        requests
    }

    /// Forgets every task, as when the service went away, and closes every window.
    pub fn reset(&mut self) -> Vec<WindowChange> {
        let changes = self
            .windows()
            .into_iter()
            .map(|task_id| WindowChange::Close { task_id })
            .collect();
        self.tasks.clear();
        self.displays.clear();
        self.closing.clear();
        self.sizes.clear();
        self.told_sizes.clear();
        self.pending.clear();
        self.told_modes.clear();
        self.cursors.clear();
        self.bands.clear();
        self.told_full_screen.clear();
        self.overlays.clear();
        // The service that comes back may have a guest that was never told.
        self.told_below_title_bar = None;
        changes
    }

    /// Makes the windows match `current`, the guest's tasks as a new watch reports them:
    /// windows of tasks that are gone close, and the others are kept and brought up to
    /// date.
    pub fn sync(&mut self, current: &[TaskEvent]) -> Vec<WindowChange> {
        let present: BTreeSet<i32> = current
            .iter()
            .filter_map(|event| match event {
                TaskEvent::Updated(task) => Some(task.task_id),
                _ => None,
            })
            .collect();
        let mut changes: Vec<WindowChange> = self
            .windows()
            .into_iter()
            .filter(|task_id| !present.contains(task_id))
            .map(|task_id| WindowChange::Close { task_id })
            .collect();
        self.tasks.retain(|task_id, _| present.contains(task_id));
        self.closing.retain(|task_id| present.contains(task_id));
        self.sizes.retain(|task_id, _| present.contains(task_id));
        self.told_sizes
            .retain(|task_id, _| present.contains(task_id));
        self.pending.retain(|task_id, _| present.contains(task_id));
        self.told_modes
            .retain(|task_id, _| present.contains(task_id));
        self.cursors.retain(|task_id, _| present.contains(task_id));
        self.bands.retain(|task_id, _| present.contains(task_id));
        self.overlays.retain(|task_id, _| present.contains(task_id));
        self.told_full_screen.clear();
        // A new watch may be of an Android that started again and was never told.
        self.told_below_title_bar = None;
        self.displays.clear();
        for event in current {
            changes.extend(self.apply(event));
        }
        changes
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn task(task_id: i32, label: &str, width: i32, height: i32) -> Task {
        Task {
            task_id,
            bounds: Bounds {
                left: 100,
                top: 50,
                right: 100 + width,
                bottom: 50 + height,
            },
            package_name: format!("com.example.app{task_id}"),
            label: label.into(),
            ..Task::default()
        }
    }

    fn updated(task_id: i32, label: &str) -> TaskEvent {
        TaskEvent::Updated(task(task_id, label, 720, 1280))
    }

    fn frames(task_id: i32, display_id: u32) -> TaskEvent {
        TaskEvent::Frames {
            task_id,
            display_id,
        }
    }

    fn opened(task_id: i32, title: &str) -> WindowChange {
        WindowChange::Open {
            task_id,
            package_name: format!("com.example.app{task_id}"),
            title: title.into(),
            width: 720,
            height: 1280,
            focused: false,
        }
    }

    /// The tone a window opens with for an app that expressed nothing about its status bar:
    /// none, so the window reads it from its pixels.
    fn dark_glyphs(task_id: i32) -> WindowChange {
        WindowChange::CaptionTone {
            task_id,
            tone: None,
        }
    }

    /// Task 144 on build 1.0.1-dev.12: the version easter egg Settings opens
    /// (`android/com.android.internal.app.PlatLogoActivity`), a task of Android's own package.
    fn system_task() -> Task {
        Task {
            package_name: "android".into(),
            label: "Android 시스템".into(),
            ..task(144, "", 675, 1199)
        }
    }

    #[test]
    fn fr3_a_task_of_androids_own_package_opens_a_window_with_its_name() {
        let mut windows = TaskWindows::new();
        let changes = windows.apply(&TaskEvent::Updated(system_task()));
        assert_eq!(
            changes.first(),
            Some(&WindowChange::Open {
                task_id: 144,
                package_name: "android".into(),
                title: "Android 시스템".into(),
                width: 675,
                height: 1199,
                focused: false,
            })
        );
        assert!(windows.has_window(144));
        windows.apply(&frames(144, 4));
        assert_eq!(windows.task_for_display(4), Some(144));
        assert!(windows.take_notes().is_empty());
    }

    #[test]
    fn fr3_a_task_that_gets_no_window_says_why() {
        let mut windows = TaskWindows::new();
        let mut bad = task(7, "Odd", 400, 600);
        bad.package_name = "..\\odd".into();
        windows.apply(&TaskEvent::Updated(bad));
        let notes = windows.take_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(notes[0].contains("task 7") && notes[0].contains("gets no window"));
    }

    #[test]
    fn fr3_a_task_whose_root_became_a_permission_dialog_keeps_its_apps_window() {
        // ONE store's task 133: the guest named the permission dialog the app opened as the
        // task's package once the app's own activities finished.
        let mut windows = TaskWindows::new();
        let mut store = task(133, "ONE store", 720, 1280);
        store.package_name = "com.skt.skaf.A000Z00040".into();
        let opened = windows.apply(&TaskEvent::Updated(store.clone()));
        assert!(opened
            .iter()
            .any(|change| matches!(change, WindowChange::Open { task_id: 133, .. })));
        let mut dialog = store.clone();
        dialog.package_name = "com.android.permissioncontroller".into();
        let changes = windows.apply(&TaskEvent::Updated(dialog));
        assert!(
            changes.iter().all(|change| !matches!(
                change,
                WindowChange::Open { .. } | WindowChange::Close { .. }
            )),
            "{changes:?}"
        );
        assert_eq!(
            windows
                .tasks
                .get(&133)
                .map(|task| task.package_name.as_str()),
            Some("com.skt.skaf.A000Z00040")
        );
        let changes = windows.apply(&TaskEvent::Updated(store));
        assert!(changes
            .iter()
            .all(|change| !matches!(change, WindowChange::Close { .. })));
    }

    #[test]
    fn fr22_a_window_hears_when_its_app_draws_its_own_title_bar_and_when_it_stops() {
        let mut windows = TaskWindows::new();
        let mut drawing = task(1, "Browser", 720, 1280);
        drawing.transparent_caption = true;
        let tabs = Bounds {
            left: 40,
            top: 0,
            right: 400,
            bottom: 31,
        };
        drawing.caption_interactive = vec![tabs];
        let opened_changes = windows.apply(&TaskEvent::Updated(drawing.clone()));
        assert!(opened_changes.contains(&WindowChange::CaptionMode {
            task_id: 1,
            below: false,
            app_drawn: true,
            interactive: vec![tabs],
        }));
        // The same again changes nothing; new controls are passed on.
        assert!(windows
            .apply(&TaskEvent::Updated(drawing.clone()))
            .iter()
            .all(|change| !matches!(change, WindowChange::CaptionMode { .. })));
        let wider = Bounds { right: 500, ..tabs };
        drawing.caption_interactive = vec![wider];
        assert!(windows
            .apply(&TaskEvent::Updated(drawing.clone()))
            .contains(&WindowChange::CaptionMode {
                task_id: 1,
                below: false,
                app_drawn: true,
                interactive: vec![wider],
            }));
        // Withdrawn, the window takes its own title bar back, and the controls no longer count.
        drawing.transparent_caption = false;
        assert!(windows
            .apply(&TaskEvent::Updated(drawing))
            .contains(&WindowChange::CaptionMode {
                task_id: 1,
                below: false,
                app_drawn: false,
                interactive: Vec::new(),
            }));
        // A window of an app that never asked hears nothing about it.
        assert!(windows
            .apply(&updated(2, "Clock"))
            .iter()
            .all(|change| !matches!(change, WindowChange::CaptionMode { .. })));
    }

    #[test]
    fn fr22_a_window_hears_that_its_task_starts_below_the_title_bar() {
        let mut windows = TaskWindows::new();
        let mut below = task(1, "Player", 720, 1280);
        below.below_title_bar = true;
        // The app also asks to draw its own title bar: the user's switch wins, so the window
        // is not app drawn and the app has no controls in the band.
        below.transparent_caption = true;
        below.caption_interactive = vec![Bounds {
            left: 40,
            top: 0,
            right: 400,
            bottom: 31,
        }];
        let opened_changes = windows.apply(&TaskEvent::Updated(below.clone()));
        assert!(opened_changes.contains(&WindowChange::CaptionMode {
            task_id: 1,
            app_drawn: false,
            interactive: Vec::new(),
            below: true,
        }));
        // Told once.
        assert!(windows
            .apply(&TaskEvent::Updated(below))
            .iter()
            .all(|change| !matches!(change, WindowChange::CaptionMode { .. })));
        // A task under the unified title bar opens with the window's own title bar, unsaid.
        assert!(windows
            .apply(&updated(2, "Notes"))
            .iter()
            .all(|change| !matches!(change, WindowChange::CaptionMode { .. })));
    }

    #[test]
    fn fr22_the_guest_is_told_which_apps_start_below_the_title_bar() {
        let now = Instant::now();
        let compat = |packages: &[&str]| TaskRequest::CaptionCompat {
            below_title_bar: packages.iter().map(|package| package.to_string()).collect(),
        };
        let app = |package: &str, below: bool| AppWindowSettings {
            package_name: package.into(),
            fixed_size: true,
            below_title_bar: below.then_some(true),
            ..AppWindowSettings::default()
        };
        let mut windows = TaskWindows::new();
        // Nothing is said before the user's settings were read.
        assert!(windows.reconfigures(now).is_empty());
        windows.set_app_windows(&[
            app("com.example.player", true),
            app("com.example.notes", false),
        ]);
        assert_eq!(
            windows.reconfigures(now),
            vec![compat(&["com.example.player"])]
        );
        // Told once, and again only when the list changes.
        assert!(windows.reconfigures(now).is_empty());
        windows.set_app_windows(&[
            app("com.example.player", true),
            app("com.example.notes", true),
        ]);
        assert_eq!(
            windows.reconfigures(now),
            vec![compat(&["com.example.notes", "com.example.player"])]
        );
        // An empty list is told too: the guest holds the one before.
        windows.set_app_windows(&[]);
        assert_eq!(windows.reconfigures(now), vec![compat(&[])]);
        assert!(windows.reconfigures(now).is_empty());
        // A new watch may be of an Android that started again, and a service that came back
        // of a guest that never heard: both are told again.
        windows.sync(&[]);
        assert_eq!(windows.reconfigures(now), vec![compat(&[])]);
        windows.reset();
        assert_eq!(windows.reconfigures(now), vec![compat(&[])]);
    }

    #[test]
    fn fr22_the_icon_becomes_a_back_button_when_the_task_can_go_back() {
        let mut windows = TaskWindows::new();
        let first = task(1, "Settings", 720, 1280);
        assert!(windows
            .apply(&TaskEvent::Updated(first.clone()))
            .iter()
            .all(|change| !matches!(change, WindowChange::CaptionBack { .. })));
        let deeper = Task {
            can_go_back: true,
            ..first.clone()
        };
        assert!(windows.apply(&TaskEvent::Updated(deeper.clone())).contains(
            &WindowChange::CaptionBack {
                task_id: 1,
                back: true
            }
        ));
        assert!(windows
            .apply(&TaskEvent::Updated(first))
            .contains(&WindowChange::CaptionBack {
                task_id: 1,
                back: false
            }));
        // A window that opens past its first screen shows the arrow from the start.
        let opened_deep = Task {
            task_id: 2,
            can_go_back: true,
            ..deeper
        };
        assert!(windows.apply(&TaskEvent::Updated(opened_deep)).contains(
            &WindowChange::CaptionBack {
                task_id: 2,
                back: true
            }
        ));
    }

    #[test]
    fn fr22_a_window_follows_its_apps_status_bar_tone() {
        use crate::tasks::StatusBarAppearance;
        let mut windows = TaskWindows::new();
        let mut dark = task(1, "Clock", 720, 1280);
        dark.status_bar = StatusBarAppearance::Dark;
        assert_eq!(
            windows.apply(&TaskEvent::Updated(dark.clone())),
            vec![
                opened(1, "Clock"),
                WindowChange::CaptionTone {
                    task_id: 1,
                    tone: Some(CaptionTone::LightGlyphs)
                }
            ]
        );
        assert!(windows.apply(&TaskEvent::Updated(dark.clone())).is_empty());
        // The app switches to a light theme.
        let mut light = dark.clone();
        light.status_bar = StatusBarAppearance::Light;
        assert_eq!(
            windows.apply(&TaskEvent::Updated(light.clone())),
            vec![WindowChange::CaptionTone {
                task_id: 1,
                tone: Some(CaptionTone::DarkGlyphs)
            }]
        );
        // A new color with the same tone changes nothing.
        light.status_bar_color = 0xFF00_0000;
        assert!(windows.apply(&TaskEvent::Updated(light)).is_empty());
    }

    #[test]
    fn fr22_a_change_of_appearance_alone_retones_the_window() {
        // A tab switch: the guest reports the app's live appearance in a task update that
        // changes nothing else (the task's description, its label and its bounds stay), and
        // the window is told at once. An appearance the app withdraws hands the window back
        // to its pixels.
        use crate::tasks::StatusBarAppearance;
        let mut windows = TaskWindows::new();
        let plain = task(1, "Store", 720, 1280);
        assert_eq!(
            windows.apply(&TaskEvent::Updated(plain.clone())),
            vec![opened(1, "Store"), dark_glyphs(1)]
        );
        for (appearance, tone) in [
            (StatusBarAppearance::Light, Some(CaptionTone::DarkGlyphs)),
            (StatusBarAppearance::Dark, Some(CaptionTone::LightGlyphs)),
            (StatusBarAppearance::Light, Some(CaptionTone::DarkGlyphs)),
            (StatusBarAppearance::Unspecified, None),
        ] {
            let mut now = plain.clone();
            now.status_bar = appearance;
            assert_eq!(
                windows.apply(&TaskEvent::Updated(now)),
                vec![WindowChange::CaptionTone { task_id: 1, tone }],
                "{appearance:?}"
            );
        }
    }

    #[test]
    fn fr22_a_windows_caption_height_becomes_its_tasks_top_inset() {
        let mut windows = TaskWindows::new();
        let band = |height| CaptionBand {
            height,
            start_width: 96,
            end_width: 276,
        };
        assert!(
            windows.window_caption(1, band(62)).is_empty(),
            "no window yet"
        );
        windows.apply(&updated(1, "Clock"));
        assert_eq!(
            windows.window_caption(1, band(62)),
            vec![TaskRequest::SetCaption {
                task_id: 1,
                height: 62,
                start_width: 96,
                end_width: 276,
            }]
        );
        windows.user_closed(1);
        assert!(
            windows.window_caption(1, band(31)).is_empty(),
            "the window closed"
        );
    }

    #[test]
    fn fr22_the_status_bar_inset_is_the_title_bar_in_the_tasks_own_pixels() {
        // The title bar is Android's status bar: the task's status bar inset is the title
        // bar's height in the task's pixels. A 200% monitor's title bar is 58 pixels.
        let band = CaptionBand {
            height: 58,
            start_width: 0,
            end_width: 0,
        };
        let inset = |task_id, height| {
            vec![TaskRequest::SetCaption {
                task_id,
                height,
                start_width: 0,
                end_width: 0,
            }]
        };
        // An app that takes the monitor's density (262 dpi) draws one pixel per window pixel:
        // 58 pixels, 35.4 dp.
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 400, 300);
        clock.density_dpi = 262;
        clock.handles_density_change = true;
        windows.apply(&TaskEvent::Updated(clock));
        windows.window_dpi(1, 192);
        assert_eq!(windows.scale(1), (1, 1));
        assert_eq!(windows.window_caption(1, band), inset(1, 58));
        // An app that keeps its density (130 dpi) is scaled by 262/130: the same band is 29 of
        // its pixels, 35.7 dp, within one of its pixels of the title bar.
        let (mut windows, _) = notes_at_100(false);
        windows.window_dpi(1, 192);
        assert_eq!(windows.scale(1), (131, 65));
        assert_eq!(windows.window_caption(1, band), inset(1, 29));
    }

    #[test]
    fn fr6_the_cursor_of_a_tasks_editor_goes_to_its_window_unchanged() {
        let mut windows = TaskWindows::new();
        let cursor = crate::ime::ImeCursor {
            left: 12,
            top: 30,
            right: 14,
            bottom: 60,
        };
        let moved = |task_id| TaskEvent::ImeCursor { task_id, cursor };
        assert!(windows.apply(&moved(1)).is_empty(), "no window yet");
        windows.apply(&updated(1, "Notes"));
        // The task's pixels are the window's, wherever the task is on the guest's display.
        assert_eq!(
            windows.apply(&moved(1)),
            vec![WindowChange::ImeCursor { task_id: 1, cursor }]
        );
        windows.user_closed(1);
        assert!(windows.apply(&moved(1)).is_empty(), "the window closed");
    }

    #[test]
    fn fr6_a_focused_editor_in_a_freeform_task_reaches_the_host_ime() {
        // The guest may name no task focused while an app's editor has its input method (the
        // window manager of Android 17 with every task freeform did): the editor still goes to
        // the task's window, which composes in it.
        let mut windows = TaskWindows::new();
        let editor = crate::ime::ImeEditor {
            kind: crate::ime::InputKind::Search,
            ..crate::ime::ImeEditor::default()
        };
        let described = TaskEvent::ImeEditor {
            task_id: 1,
            editor: editor.clone(),
        };
        assert!(windows.apply(&described).is_empty(), "no window yet");
        windows.apply(&updated(1, "Spotify"));
        assert_eq!(
            windows.apply(&described),
            vec![WindowChange::ImeEditor { task_id: 1, editor }]
        );
    }

    #[test]
    fn fr3_two_apps_get_two_windows() {
        let mut windows = TaskWindows::new();
        assert_eq!(
            windows.apply(&updated(1, "Clock")),
            vec![opened(1, "Clock"), dark_glyphs(1)]
        );
        assert_eq!(
            windows.apply(&updated(2, "Maps")),
            vec![opened(2, "Maps"), dark_glyphs(2)]
        );
        assert_eq!(windows.windows(), vec![1, 2]);
    }

    #[test]
    fn fr3_each_window_shows_only_the_frames_of_its_own_task() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.apply(&updated(2, "Maps"));
        windows.apply(&frames(1, 3));
        windows.apply(&frames(2, 4));
        assert_eq!(windows.task_for_display(3), Some(1));
        assert_eq!(windows.task_for_display(4), Some(2));
        assert_eq!(windows.routes(), vec![(3, 1), (4, 2)]);
    }

    #[test]
    fn fr3_frames_on_a_display_no_task_owns_are_never_shown() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.apply(&frames(1, 3));
        assert_eq!(windows.task_for_display(5), None);
        // A display paired with a task that has no window yet shows nothing either.
        windows.apply(&frames(9, 6));
        assert_eq!(windows.task_for_display(6), None);
        assert_eq!(windows.routes(), vec![(3, 1)]);
    }

    #[test]
    fn fr3_the_guests_whole_screen_is_never_shown_in_a_window() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.apply(&frames(1, WHOLE_SCREEN_DISPLAY));
        assert_eq!(windows.task_for_display(WHOLE_SCREEN_DISPLAY), None);
        assert!(windows.routes().is_empty());
    }

    #[test]
    fn fr3_frames_that_arrive_before_their_task_reach_its_window_once_it_opens() {
        let mut windows = TaskWindows::new();
        windows.apply(&frames(1, 3));
        assert_eq!(windows.task_for_display(3), None);
        windows.apply(&updated(1, "Clock"));
        assert_eq!(windows.task_for_display(3), Some(1));
    }

    #[test]
    fn fr3_a_display_belongs_to_the_task_that_claimed_it_last() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.apply(&updated(2, "Maps"));
        windows.apply(&frames(1, 3));
        windows.apply(&frames(2, 3));
        assert_eq!(windows.task_for_display(3), Some(2));
        assert_eq!(windows.routes(), vec![(3, 2)]);
        // A task that moves to another display leaves the old one.
        windows.apply(&frames(2, 4));
        assert_eq!(windows.task_for_display(3), None);
        assert_eq!(windows.task_for_display(4), Some(2));
    }

    #[test]
    fn fr3_a_task_that_ends_in_the_guest_closes_its_window() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.apply(&frames(1, 3));
        assert_eq!(
            windows.apply(&TaskEvent::Removed { task_id: 1 }),
            vec![WindowChange::Close { task_id: 1 }]
        );
        assert_eq!(windows.task_for_display(3), None);
        assert!(windows.windows().is_empty());
        assert!(windows.apply(&TaskEvent::Removed { task_id: 1 }).is_empty());
    }

    #[test]
    fn fr3_closing_a_window_asks_the_guest_to_end_its_task() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.apply(&frames(1, 3));
        assert_eq!(
            windows.user_closed(1),
            Some(TaskRequest::Close { task_id: 1 })
        );
        assert!(!windows.has_window(1));
        assert_eq!(windows.task_for_display(3), None);
        // Asked once; the task describing itself again before it ends opens nothing.
        assert_eq!(windows.user_closed(1), None);
        assert!(windows.apply(&updated(1, "Clock (closing)")).is_empty());
        // Its end closes no other window.
        assert!(windows.apply(&TaskEvent::Removed { task_id: 1 }).is_empty());
        assert!(
            windows.apply(&updated(1, "Clock")).len() == 2,
            "a new task 1 opens"
        );
    }

    #[test]
    fn fr3_a_close_that_did_not_reach_the_guest_opens_the_window_again() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.user_closed(1);
        assert_eq!(
            windows.close_failed(1),
            vec![opened(1, "Clock"), dark_glyphs(1)]
        );
        assert!(windows.has_window(1));
        assert!(windows.close_failed(1).is_empty());
    }

    #[test]
    fn nfr5_a_window_whose_program_crashed_opens_again_for_the_same_task() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.apply(&frames(1, 3));
        // The new window program is told everything the first one was, and the task, its
        // display and its frames stay.
        assert_eq!(windows.reopen(1), vec![opened(1, "Clock"), dark_glyphs(1)]);
        assert!(windows.has_window(1));
        assert_eq!(windows.task_for_display(3), Some(1));
        // A task without a window, or one the user is closing, opens nothing.
        assert!(windows.reopen(2).is_empty());
        windows.user_closed(1);
        assert!(windows.reopen(1).is_empty());
    }

    #[test]
    fn fr3_a_window_follows_its_tasks_label_and_size() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        assert!(windows.apply(&updated(1, "Clock")).is_empty());
        assert_eq!(
            windows.apply(&updated(1, "Alarm")),
            vec![WindowChange::Retitle {
                task_id: 1,
                title: "Alarm".into()
            }]
        );
        assert_eq!(
            windows.apply(&TaskEvent::Updated(task(1, "Alarm", 800, 600))),
            vec![WindowChange::Resize {
                task_id: 1,
                width: 800,
                height: 600
            }]
        );
        // Moving the task on the guest's display does not move or resize the window.
        let mut moved = task(1, "Alarm", 800, 600);
        moved.bounds.left += 30;
        moved.bounds.right += 30;
        assert!(windows.apply(&TaskEvent::Updated(moved)).is_empty());
    }

    #[test]
    fn fr4_a_resized_window_asks_its_task_for_the_size_and_does_not_jump_back() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        let now = Instant::now();
        assert_eq!(windows.window_dragging(1, true, now), vec![]);
        assert_eq!(
            windows.window_resized(1, (800, 1000), now),
            vec![TaskRequest::Resize {
                task_id: 1,
                width: 800,
                height: 1000,
                live: true
            }]
        );
        assert_eq!(windows.window_resized(1, (800, 1000), now), vec![]);
        // The guest catching up with an older size moves nothing.
        assert!(windows
            .apply(&TaskEvent::Updated(task(1, "Clock", 760, 1100)))
            .is_empty());
        assert!(windows
            .apply(&TaskEvent::Updated(task(1, "Clock", 800, 1000)))
            .is_empty());
        // The end of the drag reconfigures the task for the size it took live.
        assert_eq!(
            windows.window_dragging(1, false, now),
            vec![TaskRequest::Resize {
                task_id: 1,
                width: 800,
                height: 1000,
                live: false
            }]
        );
        assert!(windows
            .tick(now + super::super::window_sizing::SETTLE)
            .is_empty());
        // Nothing is asked for a window the user closed.
        windows.window_dragging(1, true, now);
        windows.window_resized(1, (900, 1000), now);
        windows.user_closed(1);
        assert_eq!(windows.window_dragging(1, false, now), vec![]);
    }

    #[test]
    fn fr4_a_window_whose_task_cannot_take_its_size_takes_the_tasks() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        let now = Instant::now();
        windows.window_resized(1, (900, 1000), now);
        windows.apply(&TaskEvent::Updated(task(1, "Clock", 850, 1000)));
        assert_eq!(
            windows.tick(now + super::super::window_sizing::SETTLE),
            vec![WindowChange::Resize {
                task_id: 1,
                width: 850,
                height: 1000
            }]
        );
    }

    #[test]
    fn fr4_a_maximised_window_reconfigures_its_task_once_the_task_has_its_size() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        let now = Instant::now();
        let resize = |live| TaskRequest::Resize {
            task_id: 1,
            width: 900,
            height: 1000,
            live,
        };
        assert_eq!(
            windows.window_resized(1, (900, 1000), now),
            vec![resize(true)]
        );
        assert_eq!(windows.reconfigures(now), vec![]);
        windows.apply(&TaskEvent::Updated(task(1, "Clock", 900, 1000)));
        assert_eq!(windows.reconfigures(now), vec![resize(false)]);
        assert_eq!(windows.reconfigures(now), vec![]);
        // Nothing is asked for a window the user closed.
        windows.window_resized(1, (700, 1000), now);
        windows.apply(&TaskEvent::Updated(task(1, "Clock", 700, 1000)));
        windows.user_closed(1);
        assert_eq!(windows.reconfigures(now), vec![]);
    }

    #[test]
    fn fr4_a_window_on_another_scale_asks_for_its_monitors_density() {
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 400, 300);
        clock.density_dpi = 130;
        clock.handles_density_change = true;
        windows.apply(&TaskEvent::Updated(clock));
        assert_eq!(
            windows.window_opened(1, 192),
            Some((
                (806, 605),
                vec![TaskRequest::SetDensity {
                    task_id: 1,
                    density_dpi: 262
                }],
                vec![]
            ))
        );
        // A move asks for nothing at once; the density follows when the move has ended.
        let (_, requests) = windows.window_dpi_changed(1, 96);
        assert!(!asks_for_density(&requests));
        assert!(windows
            .reconfigures(Instant::now())
            .contains(&TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 130
            }));
        // Nothing is asked for a window the user closed.
        windows.user_closed(1);
        assert_eq!(windows.window_dpi_changed(1, 192), (vec![], vec![]));
        assert_eq!(windows.window_resized(1, (10, 10), Instant::now()), vec![]);
        assert_eq!(windows.window_opened(1, 192), None);
    }

    #[test]
    fn fr4_a_window_hears_its_tasks_size_whenever_the_guest_changes_it() {
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 400, 300);
        // Shown at the task's size, with no limit known: nothing more to say than its tone.
        assert_eq!(windows.apply(&TaskEvent::Updated(clock.clone())).len(), 2);
        clock.max_width = 1920;
        clock.max_height = 1080;
        let size = |width, height| WindowChange::TaskSize {
            task_id: 1,
            width,
            height,
            max: Some((1920, 1080)),
            min: None,
            scale: (1, 1),
        };
        assert_eq!(
            windows.apply(&TaskEvent::Updated(clock.clone())),
            vec![size(400, 300)]
        );
        // A size the window asked for changes the task, not the window.
        windows.window_resized(1, (500, 300), Instant::now());
        clock.bounds.right += 100;
        assert_eq!(
            windows.apply(&TaskEvent::Updated(clock.clone())),
            vec![size(500, 300)]
        );
        assert!(windows.apply(&TaskEvent::Updated(clock)).is_empty());
    }

    #[test]
    fn fr4_a_window_hears_the_minimum_its_app_declares() {
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 800, 600);
        windows.apply(&TaskEvent::Updated(clock.clone()));
        clock.min_width = 700;
        clock.min_height = 500;
        assert_eq!(
            windows.apply(&TaskEvent::Updated(clock)),
            vec![WindowChange::TaskSize {
                task_id: 1,
                width: 800,
                height: 600,
                max: None,
                min: Some((700, 500)),
                scale: (1, 1),
            }]
        );
    }

    #[test]
    fn fr4_a_size_the_guest_keeps_within_the_tasks_minimum_answers_the_window_and_is_logged() {
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 800, 600);
        clock.min_width = 700;
        clock.min_height = 500;
        windows.apply(&TaskEvent::Updated(clock.clone()));
        assert_eq!(
            windows.window_resized(1, (690, 600), Instant::now()),
            vec![TaskRequest::Resize {
                task_id: 1,
                width: 690,
                height: 600,
                live: true,
            }]
        );
        assert!(windows.take_notes().is_empty());
        // The guest gives the task its smallest width: the window takes it at once.
        clock.bounds.right = clock.bounds.left + 700;
        let changes = windows.apply(&TaskEvent::Updated(clock));
        assert!(
            changes.contains(&WindowChange::Resize {
                task_id: 1,
                width: 700,
                height: 600,
            }),
            "{changes:?}"
        );
        let notes = windows.take_notes();
        assert_eq!(notes.len(), 1, "{notes:?}");
        assert!(
            notes[0].contains("task 1 took 700x600, not the 690x600"),
            "{notes:?}"
        );
        assert!(windows.take_notes().is_empty());
    }

    #[test]
    fn fr4_the_first_scale_a_window_reports_sizes_it_for_its_monitor() {
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 400, 300);
        clock.density_dpi = 130;
        clock.handles_density_change = true;
        windows.apply(&TaskEvent::Updated(clock));
        assert_eq!(
            windows.window_dpi(1, 144),
            (
                vec![WindowChange::Resize {
                    task_id: 1,
                    width: 603,
                    height: 452
                }],
                vec![TaskRequest::SetDensity {
                    task_id: 1,
                    density_dpi: 196
                }]
            )
        );
        // A later report is a move: nothing is asked while it lasts, and once it has ended the
        // task is asked for the density of the monitor it is on.
        assert_eq!(windows.window_dpi(1, 96), (vec![], vec![]));
        assert!(windows
            .reconfigures(Instant::now())
            .contains(&TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 130
            }));
    }

    /// A task window on a 100% monitor whose task draws at 130 dpi, as Android starts on a
    /// 100% main monitor; its app says it handles a change of density (`app`) or not.
    fn notes_at_100(handles_density_change: bool) -> (TaskWindows, Task) {
        let mut windows = TaskWindows::new();
        let mut notes = task(1, "Notes", 400, 300);
        notes.density_dpi = 130;
        notes.max_width = 1920;
        notes.max_height = 1920;
        notes.handles_density_change = handles_density_change;
        windows.apply(&TaskEvent::Updated(notes.clone()));
        let (changes, requests) = windows.window_dpi(1, 96);
        assert!(requests.is_empty(), "it starts at its monitor's density");
        assert!(changes.is_empty());
        (windows, notes)
    }

    fn asks_for_density(requests: &[TaskRequest]) -> bool {
        requests
            .iter()
            .any(|request| matches!(request, TaskRequest::SetDensity { .. }))
    }

    #[test]
    fn fr4_moving_to_another_scale_never_relaunches_the_app() {
        // An app that does not say it handles a change of density (scaling auto, the default):
        // a new density would relaunch it, so it keeps its density and its window is scaled.
        let (mut windows, notes) = notes_at_100(false);
        let now = Instant::now();
        let (changes, requests) = windows.window_dpi(1, 192);
        assert!(!asks_for_density(&requests), "no density for a move");
        // 262 dpi there over the task's 130.
        assert_eq!(windows.scale(1), (131, 65));
        assert_eq!(
            changes,
            vec![WindowChange::TaskSize {
                task_id: 1,
                width: 400,
                height: 300,
                max: Some((3870, 3870)),
                min: None,
                scale: (131, 65),
            }]
        );
        // The desktop doubles the window to keep its size to the eye: the same physical size
        // as a task drawn at the monitor's density, and the task keeps its own size, so nothing
        // is asked of the guest at all.
        assert_eq!(windows.window_resized(1, (806, 605), now), vec![]);
        // A resize there asks for the task's pixels, not the window's.
        assert_eq!(
            windows.window_resized(1, (1008, 605), now),
            vec![TaskRequest::Resize {
                task_id: 1,
                width: 500,
                height: 300,
                live: true,
            }]
        );
        // The title bar's band over the window is the task's inset in its own pixels.
        assert_eq!(
            windows.window_caption(
                1,
                CaptionBand {
                    height: 64,
                    start_width: 96,
                    end_width: 276
                }
            ),
            vec![TaskRequest::SetCaption {
                task_id: 1,
                height: 32,
                start_width: 48,
                end_width: 137,
            }]
        );
        // The editor's cursor is shown where the scaled window shows it.
        let cursor = crate::ime::ImeCursor {
            left: 10,
            top: 20,
            right: 12,
            bottom: 40,
        };
        assert_eq!(
            windows.apply(&TaskEvent::ImeCursor { task_id: 1, cursor }),
            vec![WindowChange::ImeCursor {
                task_id: 1,
                cursor: crate::ime::ImeCursor {
                    left: 20,
                    top: 40,
                    right: 24,
                    bottom: 80,
                },
            }]
        );
        // The guest's report of the task, at its old density, keeps the scale.
        windows.apply(&TaskEvent::Updated(notes.clone()));
        assert_eq!(windows.scale(1), (131, 65));
        // Back on the 100% monitor, the window shows the task 1:1 again, still with no density.
        let (changes, requests) = windows.window_dpi(1, 96);
        assert!(!asks_for_density(&requests));
        assert_eq!(windows.scale(1), (1, 1));
        assert!(changes
            .iter()
            .any(|change| matches!(change, WindowChange::TaskSize { scale: (1, 1), .. })));

        // System chosen for an app that does handle it: scaled the same way.
        let (mut windows, _) = notes_at_100(true);
        windows.set_app_windows(&[AppWindowSettings {
            package_name: "com.example.app1".into(),
            dpi_scaling: crate::app_windows::DpiScaling::System,
            ..AppWindowSettings::default()
        }]);
        windows.apply(&TaskEvent::Removed { task_id: 1 });
        let mut handles = task(1, "Notes", 400, 300);
        handles.density_dpi = 130;
        handles.handles_density_change = true;
        windows.apply(&TaskEvent::Updated(handles));
        windows.window_dpi(1, 96);
        let (_, requests) = windows.window_dpi(1, 144);
        assert!(!asks_for_density(&requests));
        assert_eq!(windows.scale(1), (98, 65));

        // Forced works as system until the framework change it needs is made.
        let (mut windows, _) = notes_at_100(false);
        windows.set_app_windows(&[AppWindowSettings {
            package_name: "com.example.app1".into(),
            dpi_scaling: crate::app_windows::DpiScaling::SystemForced,
            ..AppWindowSettings::default()
        }]);
        windows.apply(&TaskEvent::Removed { task_id: 1 });
        let mut forced = task(1, "Notes", 400, 300);
        forced.density_dpi = 130;
        windows.apply(&TaskEvent::Updated(forced));
        windows.window_dpi(1, 96);
        let (_, requests) = windows.window_dpi(1, 192);
        assert!(!asks_for_density(&requests));
        assert_eq!(windows.scale(1), (131, 65));
    }

    /// A YouTube window on a 100% monitor whose task draws at 130 dpi, with `settings` as the
    /// user's window settings for every app.
    fn youtube_at_100(settings: &[AppWindowSettings]) -> TaskWindows {
        let mut windows = TaskWindows::new();
        windows.set_app_windows(settings);
        let mut youtube = task(1, "YouTube", 400, 300);
        youtube.package_name = "com.google.android.youtube".into();
        youtube.density_dpi = 130;
        youtube.max_width = 1920;
        youtube.max_height = 1920;
        youtube.handles_density_change = true;
        windows.apply(&TaskEvent::Updated(youtube));
        let (_, requests) = windows.window_dpi(1, 96);
        assert!(requests.is_empty(), "it starts at its monitor's density");
        windows
    }

    #[test]
    fn fr4_an_app_mode_window_dragged_to_another_scale_keeps_its_pixel_size_and_its_task_takes_it_1_to_1(
    ) {
        // An app that handles a change of density, dragged from 100% to 200%.
        let (mut windows, mut notes) = notes_at_100(true);
        let now = Instant::now();
        assert_eq!(windows.window_dragging(1, true, now), vec![]);
        let (changes, requests) = windows.window_dpi(1, 192);
        assert!(!asks_for_density(&requests), "never during the drag");
        assert!(!changes
            .iter()
            .any(|change| matches!(change, WindowChange::Resize { .. })));
        assert_eq!(windows.scale(1), (131, 65));
        // The desktop gives the window its size on the new monitor; the task keeps its own.
        assert_eq!(windows.window_resized(1, (806, 605), now), vec![]);
        // The drag ends: the density of the monitor and the window's size in pixels, 1:1.
        assert_eq!(
            windows.window_dragging(1, false, now),
            vec![
                TaskRequest::SetDensity {
                    task_id: 1,
                    density_dpi: 262
                },
                TaskRequest::Resize {
                    task_id: 1,
                    width: 806,
                    height: 605,
                    live: true,
                },
            ]
        );
        assert!(!asks_for_density(&windows.reconfigures(now)), "once");
        // The window is never resized on the way: neither by the guest's reports nor by a tick.
        notes.density_dpi = 262;
        notes.bounds.right = notes.bounds.left + 806;
        notes.bounds.bottom = notes.bounds.top + 605;
        let changes = windows.apply(&TaskEvent::Updated(notes));
        assert!(!changes
            .iter()
            .any(|change| matches!(change, WindowChange::Resize { .. })));
        assert!(!windows
            .tick(now + SETTLE * 2)
            .iter()
            .any(|change| matches!(change, WindowChange::Resize { .. })));
        assert_eq!(windows.task_size(1), Some((806, 605)));
        assert_eq!(windows.scale(1), (1, 1));
    }

    #[test]
    fn fr4_a_system_mode_window_is_unchanged_by_the_end_of_a_move() {
        let (mut windows, _) = notes_at_100(false);
        let now = Instant::now();
        windows.window_dragging(1, true, now);
        windows.window_dpi(1, 192);
        assert_eq!(windows.window_resized(1, (806, 605), now), vec![]);
        assert_eq!(windows.window_dragging(1, false, now), vec![]);
        assert!(!asks_for_density(&windows.reconfigures(now)));
        assert_eq!(windows.scale(1), (131, 65));
        assert_eq!(windows.task_size(1), Some((400, 300)));
    }

    #[test]
    fn fr4_youtube_scales_the_system_way_by_default() {
        // With no choice of the user's, or with auto chosen, a move to a monitor of another
        // scale gives YouTube no new density, though it says it handles one: it is scaled.
        let auto = AppWindowSettings {
            package_name: "com.google.android.youtube".into(),
            fixed_size: true,
            dpi_scaling: crate::app_windows::DpiScaling::Auto,
            ..AppWindowSettings::default()
        };
        for settings in [vec![], vec![auto]] {
            let mut windows = youtube_at_100(&settings);
            let now = Instant::now();
            let (_, requests) = windows.window_dpi(1, 192);
            assert!(!asks_for_density(&requests));
            // 262 dpi there over the task's 130.
            assert_eq!(windows.scale(1), (131, 65));
            // A drag that ends there asks for no density either.
            assert!(!asks_for_density(&windows.window_dragging(1, true, now)));
            assert!(!asks_for_density(&windows.window_dragging(1, false, now)));
            let (_, requests) = windows.window_dpi(1, 120);
            assert!(!asks_for_density(&requests));
        }
    }

    #[test]
    fn fr4_the_users_choice_wins_over_the_built_in_list() {
        // The user chose the app way for YouTube: a move gives it the new density at once.
        let mut windows = youtube_at_100(&[AppWindowSettings {
            package_name: "com.google.android.youtube".into(),
            dpi_scaling: crate::app_windows::DpiScaling::App,
            ..AppWindowSettings::default()
        }]);
        let (_, requests) = windows.window_dpi(1, 192);
        assert!(!asks_for_density(&requests), "not before the move ends");
        windows.window_resized(1, (806, 605), Instant::now());
        assert!(windows
            .reconfigures(Instant::now())
            .contains(&TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 262
            }));
        youtube_updated(&mut windows, 806, 605, 262);
        assert_eq!(windows.scale(1), (1, 1));
    }

    /// YouTube's task as the guest reports it at `width` x `height` and `density_dpi`.
    fn youtube_updated(windows: &mut TaskWindows, width: i32, height: i32, density_dpi: u32) {
        let mut youtube = task(1, "YouTube", width, height);
        youtube.package_name = "com.google.android.youtube".into();
        youtube.density_dpi = density_dpi;
        youtube.max_width = 1920;
        youtube.max_height = 1920;
        youtube.handles_density_change = true;
        windows.apply(&TaskEvent::Updated(youtube));
    }

    #[test]
    fn fr6_on_a_monitor_of_another_scale_the_editors_cursor_is_where_the_window_shows_it() {
        // Notes keeps 130 dpi, the density of a monitor at 100%.
        let (mut windows, notes) = notes_at_100(false);
        let cursor = crate::ime::ImeCursor {
            left: 100,
            top: 200,
            right: 102,
            bottom: 240,
        };
        // At 100% the window shows the task 1:1, and the cursor where the task has it.
        assert_eq!(
            windows.apply(&TaskEvent::ImeCursor { task_id: 1, cursor }),
            vec![WindowChange::ImeCursor { task_id: 1, cursor }]
        );
        // The window moves to a monitor at 125% (164 dpi). The guest does not move the cursor,
        // but the window now shows it 164 window pixels per 130: the candidate window must
        // follow without waiting for the next keystroke.
        let (changes, _) = windows.window_dpi(1, 120);
        assert_eq!(windows.scale(1), (82, 65));
        assert!(
            changes.contains(&WindowChange::ImeCursor {
                task_id: 1,
                cursor: crate::ime::ImeCursor {
                    left: 126,
                    top: 252,
                    right: 128,
                    bottom: 302,
                },
            }),
            "{changes:?}"
        );
        // Back at 100%, it is where the task has it again.
        let (changes, _) = windows.window_dpi(1, 96);
        assert!(
            changes.contains(&WindowChange::ImeCursor { task_id: 1, cursor }),
            "{changes:?}"
        );
        // A window that closes forgets it: the task's next window hears of no cursor until the
        // guest reports one.
        windows.apply(&TaskEvent::Removed { task_id: 1 });
        windows.apply(&TaskEvent::Updated(notes));
        windows.window_dpi(1, 96);
        let (changes, _) = windows.window_dpi(1, 120);
        assert!(
            !changes
                .iter()
                .any(|change| matches!(change, WindowChange::ImeCursor { .. })),
            "{changes:?}"
        );
    }

    #[test]
    fn fr22_a_window_at_a_new_scale_gives_its_task_the_caption_inset_in_the_tasks_pixels() {
        let (mut windows, notes) = notes_at_100(false);
        let now = Instant::now();
        // The window reports the title bar of a monitor at 125% before the move itself reaches
        // the window manager: the inset is converted at the old scale, 1:1.
        let band = CaptionBand {
            height: 50,
            start_width: 60,
            end_width: 172,
        };
        assert_eq!(
            windows.window_caption(1, band),
            vec![TaskRequest::SetCaption {
                task_id: 1,
                height: 50,
                start_width: 60,
                end_width: 172,
            }]
        );
        assert_eq!(windows.reconfigures(now), vec![]);
        // Then the move: the window shows the task at 164 per 130, and the same band is fewer
        // of the task's pixels. The inset is given again without the band changing.
        let (_, requests) = windows.window_dpi(1, 120);
        assert_eq!(windows.scale(1), (82, 65));
        assert_eq!(
            requests,
            vec![TaskRequest::SetCaption {
                task_id: 1,
                height: 40,
                start_width: 48,
                end_width: 136,
            }]
        );
        // Once.
        assert_eq!(windows.reconfigures(now), vec![]);
        // A band reported at the new scale is converted at it, and not given again.
        assert_eq!(
            windows.window_caption(1, band),
            vec![TaskRequest::SetCaption {
                task_id: 1,
                height: 40,
                start_width: 48,
                end_width: 136,
            }]
        );
        assert_eq!(windows.reconfigures(now), vec![]);
        // A closed window's band is forgotten: the task's next window has its own.
        windows.apply(&TaskEvent::Removed { task_id: 1 });
        windows.apply(&TaskEvent::Updated(notes));
        windows.window_dpi(1, 96);
        let (_, requests) = windows.window_dpi(1, 120);
        assert!(
            !requests
                .iter()
                .any(|request| matches!(request, TaskRequest::SetCaption { .. })),
            "{requests:?}"
        );
        assert_eq!(windows.reconfigures(now), vec![]);
    }

    #[test]
    fn fr4_an_app_mode_task_gets_the_new_monitors_density() {
        // An app that says it handles a change of density redraws at the new density, which
        // Android does without relaunching it, and its window stays 1:1.
        let (mut windows, mut notes) = notes_at_100(true);
        let now = Instant::now();
        let (_, requests) = windows.window_dpi(1, 192);
        assert!(!asks_for_density(&requests), "not before the move ends");
        // The desktop doubles the window (400x300 at the scale 262 over 130 is 806x605); the task, still at 130 dpi, is shown scaled into it.
        assert_eq!(windows.window_resized(1, (806, 605), now), vec![]);
        // The move has ended: the monitor's density, and the window's size 1:1.
        let requests = windows.reconfigures(now);
        assert!(requests.contains(&TaskRequest::SetDensity {
            task_id: 1,
            density_dpi: 262
        }));
        assert!(requests.contains(&TaskRequest::Resize {
            task_id: 1,
            width: 806,
            height: 605,
            live: true,
        }));
        // Another activity on top that does not say so: the task keeps the density it has
        // from now on, and a later move scales the window instead.
        notes.density_dpi = 262;
        notes.bounds.right = notes.bounds.left + 806;
        notes.bounds.bottom = notes.bounds.top + 605;
        notes.handles_density_change = false;
        windows.apply(&TaskEvent::Updated(notes));
        assert_eq!(windows.scale(1), (1, 1));
        let (_, requests) = windows.window_dpi(1, 96);
        assert!(!asks_for_density(&requests));
        assert!(!asks_for_density(&windows.reconfigures(now)));
        assert_eq!(windows.scale(1), (65, 131));
    }

    #[test]
    fn fr4_an_explicit_app_choice_gives_the_density_to_an_app_that_does_not_handle_it() {
        // The user chose the app mode for an app that does not say it handles a change of
        // density: it is given the new density anyway, and Android recreates its activity and
        // restores its state, so its text stays sharp. Its window stays 1:1.
        let (mut windows, _) = notes_at_100(false);
        windows.set_app_windows(&[AppWindowSettings {
            package_name: "com.example.app1".into(),
            dpi_scaling: crate::app_windows::DpiScaling::App,
            ..AppWindowSettings::default()
        }]);
        windows.apply(&TaskEvent::Removed { task_id: 1 });
        let mut notes = task(1, "Notes", 400, 300);
        notes.density_dpi = 130;
        notes.max_width = 1920;
        notes.max_height = 1920;
        windows.apply(&TaskEvent::Updated(notes.clone()));
        let (_, requests) = windows.window_dpi(1, 96);
        assert!(
            !asks_for_density(&requests),
            "it starts at its monitor's density"
        );
        let now = Instant::now();
        let (_, requests) = windows.window_dpi(1, 192);
        assert!(!asks_for_density(&requests), "not before the move ends");
        assert!(windows
            .reconfigures(now)
            .contains(&TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 262
            }));
        // Its next activity does not say so either, and the choice still holds.
        notes.density_dpi = 262;
        windows.apply(&TaskEvent::Updated(notes));
        assert_eq!(windows.scale(1), (1, 1));
        let (_, requests) = windows.window_dpi(1, 96);
        assert!(!asks_for_density(&requests));
        assert!(windows
            .reconfigures(now)
            .contains(&TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 130
            }));

        // Auto and system never give such an app another density.
        for scaling in [
            crate::app_windows::DpiScaling::Auto,
            crate::app_windows::DpiScaling::System,
        ] {
            let (mut windows, _) = notes_at_100(false);
            windows.set_app_windows(&[AppWindowSettings {
                package_name: "com.example.app1".into(),
                dpi_scaling: scaling,
                ..AppWindowSettings::default()
            }]);
            for dpi in [192, 144, 96] {
                let (_, requests) = windows.window_dpi(1, dpi);
                assert!(!asks_for_density(&requests), "{scaling:?} at {dpi}");
                let requests = windows.reconfigures(Instant::now());
                assert!(!asks_for_density(&requests), "{scaling:?} at {dpi}");
            }
        }
    }

    #[test]
    fn fr4_a_window_may_not_grow_past_what_the_guest_can_draw() {
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 400, 300);
        assert_eq!(windows.max_client_size(1), None);
        windows.apply(&TaskEvent::Updated(clock.clone()));
        assert_eq!(windows.max_client_size(1), None);
        clock.max_width = 1920;
        clock.max_height = 1080;
        windows.apply(&TaskEvent::Updated(clock));
        assert_eq!(windows.max_client_size(1), Some((1920, 1080)));
    }

    #[test]
    fn nfr5_losing_the_service_closes_every_window() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.apply(&updated(2, "Maps"));
        windows.apply(&frames(2, 4));
        assert_eq!(
            windows.reset(),
            vec![
                WindowChange::Close { task_id: 1 },
                WindowChange::Close { task_id: 2 }
            ]
        );
        assert_eq!(windows.task_for_display(4), None);
        assert!(windows.windows().is_empty());
    }

    #[test]
    fn fr3_a_new_watch_keeps_the_windows_of_tasks_that_still_exist() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.apply(&updated(2, "Maps"));
        windows.apply(&frames(2, 4));
        let changes = windows.sync(&[updated(2, "Maps 2"), updated(3, "Mail"), frames(3, 5)]);
        assert_eq!(
            changes,
            vec![
                WindowChange::Close { task_id: 1 },
                WindowChange::Retitle {
                    task_id: 2,
                    title: "Maps 2".into()
                },
                opened(3, "Mail"),
                dark_glyphs(3),
            ]
        );
        assert_eq!(windows.routes(), vec![(5, 3)]);
    }

    #[test]
    fn fr3_a_window_is_titled_and_sized_even_when_the_guest_leaves_it_out() {
        let mut nameless = task(4, " ", 0, 0);
        assert_eq!(window_title(&nameless), "com.example.app4");
        nameless.package_name.clear();
        assert_eq!(window_title(&nameless), "Task 4");
        assert_eq!(window_size(&nameless.bounds), DEFAULT_WINDOW_SIZE);
    }

    #[test]
    fn fr3_a_window_taller_than_the_screen_opens_smaller_with_the_same_shape() {
        assert_eq!(fit_within((720, 1280), (1920, 1040)), (585, 1040));
        assert_eq!(fit_within((800, 600), (1920, 1040)), (800, 600));
        assert_eq!(fit_within((4000, 100), (1000, 1000)), (1000, 25));
        assert_eq!(fit_within((10, 10), (0, 0)), (1, 1));
    }

    fn portrait_app(task_id: i32, width: i32, height: i32) -> Task {
        Task {
            orientation: TaskOrientation::Portrait,
            density_dpi: 130,
            max_width: 1920,
            max_height: 1080,
            ..task(task_id, "Reader", width, height)
        }
    }

    #[test]
    fn fr30_a_portrait_locked_task_opens_as_a_window_taller_than_it_is_wide() {
        let mut windows = TaskWindows::new();
        let now = Instant::now();
        // The guest gave the task a landscape size: the window waits while the task is given
        // the portrait one, so it opens with frames of its own size.
        assert!(windows
            .apply(&TaskEvent::Updated(portrait_app(1, 1000, 600)))
            .is_empty());
        assert!(!windows.has_window(1));
        assert_eq!(
            windows.reconfigures(now),
            vec![TaskRequest::Resize {
                task_id: 1,
                width: 600,
                height: 1000,
                live: false,
            }]
        );
        assert!(windows.reconfigures(now).is_empty());
        let changes = windows.apply(&TaskEvent::Updated(portrait_app(1, 600, 1000)));
        let Some(WindowChange::Open { width, height, .. }) = changes.first() else {
            panic!("expected the window to open, got {changes:?}");
        };
        assert!(height > width, "{width} x {height}");
        assert!(windows.has_window(1));
    }

    #[test]
    fn fr30_a_portrait_task_that_is_portrait_already_opens_at_once() {
        let mut windows = TaskWindows::new();
        let changes = windows.apply(&TaskEvent::Updated(portrait_app(1, 600, 1000)));
        assert!(matches!(changes[0], WindowChange::Open { .. }));
        assert!(windows.reconfigures(Instant::now()).is_empty());
    }

    #[test]
    fn fr30_a_window_whose_task_does_not_take_its_start_size_opens_anyway() {
        let mut windows = TaskWindows::new();
        let now = Instant::now();
        windows.apply(&TaskEvent::Updated(portrait_app(1, 1000, 600)));
        windows.reconfigures(now);
        assert!(windows.tick(now).is_empty());
        let changes = windows.tick(now + SETTLE);
        assert!(
            matches!(
                changes[0],
                WindowChange::Open {
                    width: 1000,
                    height: 600,
                    ..
                }
            ),
            "{changes:?}"
        );
    }

    #[test]
    fn fr30_an_apps_start_size_and_fixed_size_come_from_the_users_settings() {
        let mut windows = TaskWindows::new();
        windows.set_app_windows(&[AppWindowSettings {
            package_name: "com.example.app1".into(),
            fixed_size: true,
            start_size_dp: Some((400, 300)),
            ..AppWindowSettings::default()
        }]);
        let app = Task {
            density_dpi: 320,
            ..task(1, "Notes", 720, 1280)
        };
        windows.apply(&TaskEvent::Updated(app.clone()));
        // 400 x 300 dp at 320 dpi. The guest also hears that no app starts below the title bar.
        assert_eq!(
            windows.reconfigures(Instant::now()),
            vec![
                TaskRequest::CaptionCompat {
                    below_title_bar: Vec::new(),
                },
                TaskRequest::Resize {
                    task_id: 1,
                    width: 800,
                    height: 600,
                    live: false,
                }
            ]
        );
        let changes = windows.apply(&TaskEvent::Updated(Task {
            density_dpi: 320,
            ..task(1, "Notes", 800, 600)
        }));
        assert!(changes.contains(&WindowChange::Mode {
            task_id: 1,
            mode: WindowMode::Normal,
            resizable: false,
        }));
        // Resetting the app's settings lets its window be resized again at once.
        assert_eq!(
            windows.set_app_windows(&[]),
            vec![WindowChange::Mode {
                task_id: 1,
                mode: WindowMode::Normal,
                resizable: true,
            }]
        );
    }

    #[test]
    fn fr30_inside_the_window_keeps_the_caption_and_gives_the_task_no_band() {
        use crate::app_windows::FullScreenDefaults;
        use crate::tasks::FullScreenRequest;
        let now = Instant::now();
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Shorts"));
        let band = CaptionBand {
            height: 32,
            start_width: 48,
            end_width: 138,
        };
        let given = |band: CaptionBand| TaskRequest::SetCaption {
            task_id: 1,
            height: band.height,
            start_width: band.start_width,
            end_width: band.end_width,
        };
        assert_eq!(windows.window_caption(1, band), vec![given(band)]);
        // Hiding both bars alone stays in the window by default: an ordinary window, which
        // keeps its title bar, and a task with no band under it.
        let immersive = Task {
            fullscreen_request: FullScreenRequest::BarsHidden,
            ..task(1, "Shorts", 720, 1280)
        };
        assert!(!windows
            .apply(&TaskEvent::Updated(immersive.clone()))
            .iter()
            .any(|change| matches!(change, WindowChange::Mode { .. })));
        assert_eq!(
            windows.reconfigures(now),
            vec![given(CaptionBand::default())]
        );
        // A band the window reports again while the app fills it is not given either.
        assert_eq!(
            windows.window_caption(1, band),
            vec![given(CaptionBand::default())]
        );
        // The band comes back when the app shows its bars again.
        windows.apply(&updated(1, "Shorts"));
        assert_eq!(windows.reconfigures(now), vec![given(band)]);
        // Set to the whole screen, the same request fills the monitor.
        windows.apply(&TaskEvent::Updated(immersive));
        windows.reconfigures(now);
        assert_eq!(
            windows.set_full_screen_defaults(FullScreenDefaults {
                immersive: FullScreenPlace::Screen,
                ..FullScreenDefaults::default()
            }),
            vec![WindowChange::Mode {
                task_id: 1,
                mode: WindowMode::Fullscreen,
                resizable: true,
            }]
        );
    }

    #[test]
    fn fr30_a_clear_request_fills_the_screen_unless_the_app_is_set_to_its_window() {
        use crate::tasks::FullScreenRequest;
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Video"));
        let full = Task {
            fullscreen_request: FullScreenRequest::Explicit,
            ..task(1, "Video", 720, 1280)
        };
        assert!(windows
            .apply(&TaskEvent::Updated(full))
            .contains(&WindowChange::Mode {
                task_id: 1,
                mode: WindowMode::Fullscreen,
                resizable: true,
            }));
        // The app's own choice wins over the default, at once for the window open already.
        let package = windows.package_of(1).unwrap().to_string();
        assert_eq!(
            windows.set_app_windows(&[AppWindowSettings {
                package_name: package,
                fullscreen: FullScreenPlace::Window,
                ..AppWindowSettings::default()
            }]),
            vec![WindowChange::Mode {
                task_id: 1,
                mode: WindowMode::Normal,
                resizable: true,
            }]
        );
    }

    #[test]
    fn fr30_the_guest_hears_to_keep_a_task_that_leaves_freeform_inside_its_window() {
        let now = Instant::now();
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Player"));
        // The default fills the screen, which the guest does without being told.
        assert!(windows.reconfigures(now).is_empty());
        let package = windows.package_of(1).unwrap().to_string();
        windows.set_app_windows(&[AppWindowSettings {
            package_name: package.clone(),
            fullscreen: FullScreenPlace::Window,
            ..AppWindowSettings::default()
        }]);
        assert_eq!(
            windows.reconfigures(now),
            vec![
                TaskRequest::CaptionCompat {
                    below_title_bar: Vec::new(),
                },
                TaskRequest::SetFullScreen {
                    task_id: 1,
                    in_window: true,
                }
            ]
        );
        // Told once.
        assert!(windows.reconfigures(now).is_empty());
        // Back to the whole screen, by the app's choice or by the default for every app.
        windows.set_app_windows(&[]);
        windows.set_full_screen_defaults(crate::app_windows::FullScreenDefaults {
            fullscreen: FullScreenPlace::Window,
            immersive: FullScreenPlace::Window,
        });
        assert!(windows.reconfigures(now).is_empty());
        windows.set_full_screen_defaults(crate::app_windows::FullScreenDefaults::default());
        assert_eq!(
            windows.reconfigures(now),
            vec![TaskRequest::SetFullScreen {
                task_id: 1,
                in_window: false,
            }]
        );
    }

    #[test]
    fn fr30_full_screen_and_picture_in_picture_requests_reach_the_window() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Video"));
        let full = Task {
            fullscreen_request: crate::tasks::FullScreenRequest::Explicit,
            ..task(1, "Video", 720, 1280)
        };
        assert_eq!(
            windows.apply(&TaskEvent::Updated(full)),
            vec![WindowChange::Mode {
                task_id: 1,
                mode: WindowMode::Fullscreen,
                resizable: true,
            }]
        );
        let pip = Task {
            picture_in_picture: true,
            ..task(1, "Video", 720, 1280)
        };
        assert_eq!(
            windows.apply(&TaskEvent::Updated(pip)),
            vec![WindowChange::Mode {
                task_id: 1,
                mode: WindowMode::PictureInPicture,
                resizable: true,
            }]
        );
        assert_eq!(
            windows.apply(&updated(1, "Video")),
            vec![WindowChange::Mode {
                task_id: 1,
                mode: WindowMode::Normal,
                resizable: true,
            }]
        );
    }

    use crate::tasks::{OverlayWindow, OVERLAY_ID_BASE};

    const LYRICS: i32 = OVERLAY_ID_BASE + 1;

    /// Floating lyrics at (`left`, `top`) of a 3840 pixel display drawn at 262 dpi.
    fn lyrics(left: i32, top: i32, width: i32, height: i32) -> Task {
        Task {
            task_id: LYRICS,
            bounds: Bounds {
                left,
                top,
                right: left + width,
                bottom: top + height,
            },
            package_name: "skplanet.musicmate".into(),
            label: "FLO".into(),
            density_dpi: 262,
            max_width: 3840,
            max_height: 3840,
            overlay: Some(OverlayWindow {
                window_type: 2038,
                focusable: false,
                touchable: true,
            }),
            ..Task::default()
        }
    }

    fn placed(left: i32, top: i32, focusable: bool, touchable: bool) -> WindowChange {
        WindowChange::Overlay {
            task_id: LYRICS,
            left,
            top,
            display: (3840, 3840),
            focusable,
            touchable,
        }
    }

    #[test]
    fn fr41_an_overlay_opens_a_window_of_its_own() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Player"));
        assert_eq!(
            windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138))),
            vec![
                WindowChange::Open {
                    task_id: LYRICS,
                    package_name: "skplanet.musicmate".into(),
                    title: "FLO".into(),
                    width: 508,
                    height: 138,
                    focused: false,
                },
                WindowChange::Mode {
                    task_id: LYRICS,
                    mode: WindowMode::Overlay,
                    resizable: false,
                },
                placed(158, 82, false, true),
            ]
        );
        assert!(windows.has_window(LYRICS));
        assert!(windows.has_overlay_window(LYRICS));
        // It is shown, so Android stays awake for it, but it is no app to open again.
        assert_eq!(windows.windows(), vec![1, LYRICS]);
        assert_eq!(windows.window_packages(), vec!["com.example.app1"]);
        // Its frames reach its window like a task's.
        windows.apply(&frames(LYRICS, 4));
        assert_eq!(windows.task_for_display(4), Some(LYRICS));
    }

    #[test]
    fn fr41_an_overlay_window_follows_where_its_app_moves_it() {
        let mut windows = TaskWindows::new();
        windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)));
        // The same report again changes nothing.
        assert!(windows
            .apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)))
            .is_empty());
        // The app drags it: only its place is news.
        assert_eq!(
            windows.apply(&TaskEvent::Updated(lyrics(258, 122, 508, 138))),
            vec![placed(258, 122, false, true)]
        );
    }

    #[test]
    fn fr41_an_overlay_window_takes_the_size_its_app_gives_it() {
        let mut windows = TaskWindows::new();
        windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)));
        assert_eq!(
            windows.apply(&TaskEvent::Updated(lyrics(158, 82, 700, 300))),
            vec![
                WindowChange::Resize {
                    task_id: LYRICS,
                    width: 700,
                    height: 300,
                },
                WindowChange::TaskSize {
                    task_id: LYRICS,
                    width: 700,
                    height: 300,
                    max: None,
                    min: None,
                    scale: (1, 1),
                },
            ]
        );
    }

    #[test]
    fn fr41_an_overlay_is_shown_at_the_density_of_its_monitor() {
        let mut windows = TaskWindows::new();
        windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)));
        // On the monitor Android's display takes its density from (200%), pixel for pixel.
        assert_eq!(windows.window_dpi(LYRICS, 192), (Vec::new(), Vec::new()));
        // Moved to a 125% monitor (164 dpi): the same size to the eye, and the guest is asked
        // for nothing.
        let (changes, requests) = windows.window_dpi_changed(LYRICS, 120);
        assert!(requests.is_empty());
        assert_eq!(
            changes,
            vec![
                WindowChange::Resize {
                    task_id: LYRICS,
                    width: 318,
                    height: 86,
                },
                WindowChange::TaskSize {
                    task_id: LYRICS,
                    width: 508,
                    height: 138,
                    max: None,
                    min: None,
                    scale: (82, 131),
                },
            ]
        );
        assert_eq!(windows.scale(LYRICS), (82, 131));
    }

    #[test]
    fn fr41_the_host_never_closes_resizes_or_reconfigures_an_overlay() {
        let mut windows = TaskWindows::new();
        let now = Instant::now();
        windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)));
        // No close button: a close its window reports anyway ends nothing.
        assert_eq!(windows.user_closed(LYRICS), None);
        assert!(windows.has_window(LYRICS));
        assert!(windows.window_resized(LYRICS, (300, 300), now).is_empty());
        assert!(windows
            .window_caption(
                LYRICS,
                CaptionBand {
                    height: 32,
                    start_width: 40,
                    end_width: 138,
                }
            )
            .is_empty());
        assert!(windows.window_dragging(LYRICS, true, now).is_empty());
        assert!(windows.window_dragging(LYRICS, false, now).is_empty());
        assert!(windows.reconfigures(now).is_empty());
        assert!(windows.tick(now + SETTLE * 2).is_empty());
    }

    #[test]
    fn fr41_an_overlay_window_goes_when_the_app_removes_it_or_android_stops() {
        let mut windows = TaskWindows::new();
        windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)));
        windows.apply(&frames(LYRICS, 4));
        assert_eq!(
            windows.apply(&TaskEvent::Removed { task_id: LYRICS }),
            vec![WindowChange::Close { task_id: LYRICS }]
        );
        assert!(!windows.has_window(LYRICS));
        assert_eq!(windows.task_for_display(4), None);
        // The service went away with Android: every overlay's window closes too.
        windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)));
        assert_eq!(
            windows.reset(),
            vec![WindowChange::Close { task_id: LYRICS }]
        );
        assert!(!windows.is_overlay(LYRICS));
        // And one a new watch no longer lists.
        windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)));
        assert_eq!(
            windows.sync(&[]),
            vec![WindowChange::Close { task_id: LYRICS }]
        );
    }

    #[test]
    fn fr41_a_full_display_overlay_gets_no_window() {
        let mut windows = TaskWindows::new();
        // A screen filter: the whole 3840 by 3840 display, taking no input.
        let filter = Task {
            overlay: Some(OverlayWindow {
                window_type: 2038,
                focusable: false,
                touchable: false,
            }),
            ..lyrics(0, 0, 3840, 3840)
        };
        assert!(filter.covers_display());
        assert!(windows
            .apply(&TaskEvent::Updated(filter.clone()))
            .is_empty());
        assert!(!windows.has_window(LYRICS));
        assert!(windows.windows().is_empty());
        // Nor one that takes input, which would take every click on the desktop.
        let wall = lyrics(-10, -10, 3900, 3900);
        assert!(wall.covers_display());
        assert!(windows.apply(&TaskEvent::Updated(wall)).is_empty());
        // Whatever the hwcomposer says of its frames, they reach no window.
        windows.apply(&frames(LYRICS, 4));
        assert_eq!(windows.task_for_display(4), None);
        // Once its app makes it smaller it is a window like any other overlay, and when it
        // grows over the display again the window closes.
        assert_eq!(
            windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)))[0],
            WindowChange::Open {
                task_id: LYRICS,
                package_name: "skplanet.musicmate".into(),
                title: "FLO".into(),
                width: 508,
                height: 138,
                focused: false,
            }
        );
        assert_eq!(
            windows.apply(&TaskEvent::Updated(filter)),
            vec![WindowChange::Close { task_id: LYRICS }]
        );
        assert!(windows
            .apply(&TaskEvent::Removed { task_id: LYRICS })
            .is_empty());
        // A display-wide bar, or an overlay on a display of unknown size, is no such overlay.
        assert!(!lyrics(0, 0, 3840, 200).covers_display());
        let no_display = Task {
            max_width: 0,
            max_height: 0,
            ..lyrics(0, 0, 3840, 3840)
        };
        assert!(!no_display.covers_display());
        assert!(!task(1, "Player", 3840, 3840).covers_display());
    }

    #[test]
    fn fr41_an_overlay_never_opens_focused_and_says_whether_it_may_take_the_focus() {
        let mut windows = TaskWindows::new();
        // Even one the guest calls focused opens without taking the user's focus.
        let focused = Task {
            focused: true,
            ..lyrics(158, 82, 508, 138)
        };
        let changes = windows.apply(&TaskEvent::Updated(focused));
        assert!(matches!(
            changes[0],
            WindowChange::Open { focused: false, .. }
        ));
        assert_eq!(changes[2], placed(158, 82, false, true));
        // The app makes it focusable (a chat head opening its text field): the window hears.
        let chat = Task {
            overlay: Some(OverlayWindow {
                window_type: 2038,
                focusable: true,
                touchable: true,
            }),
            ..lyrics(158, 82, 508, 138)
        };
        assert_eq!(
            windows.apply(&TaskEvent::Updated(chat)),
            vec![placed(158, 82, true, true)]
        );
    }

    #[test]
    fn fr41_several_overlays_each_have_a_window() {
        let mut windows = TaskWindows::new();
        windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)));
        let second = Task {
            task_id: LYRICS + 1,
            package_name: "com.example.chat".into(),
            ..lyrics(3000, 500, 160, 160)
        };
        let changes = windows.apply(&TaskEvent::Updated(second));
        assert!(matches!(
            &changes[0],
            WindowChange::Open { task_id, package_name, .. }
                if *task_id == LYRICS + 1 && package_name == "com.example.chat"
        ));
        assert_eq!(windows.windows(), vec![LYRICS, LYRICS + 1]);
        // One going leaves the other.
        windows.apply(&TaskEvent::Removed { task_id: LYRICS });
        assert_eq!(windows.windows(), vec![LYRICS + 1]);
    }

    #[test]
    fn fr41_an_overlay_window_that_crashed_opens_again() {
        let mut windows = TaskWindows::new();
        windows.apply(&TaskEvent::Updated(lyrics(158, 82, 508, 138)));
        windows.window_dpi(LYRICS, 120);
        let again = windows.reopen(LYRICS);
        assert!(matches!(again[0], WindowChange::Open { width: 508, .. }));
        assert_eq!(again[2], placed(158, 82, false, true));
    }

    #[test]
    fn fr4_the_flo_drag_ends_at_the_size_the_user_let_go_at() {
        // The trace of FLO (task 134) dragged to 677x987, 16:47:51.767 to 52.375.
        let live = |width, height| TaskRequest::Resize {
            task_id: 1,
            width,
            height,
            live: true,
        };
        let end = TaskRequest::Resize {
            task_id: 1,
            width: 677,
            height: 987,
            live: false,
        };
        let resizes = |changes: &[WindowChange]| {
            changes
                .iter()
                .any(|change| matches!(change, WindowChange::Resize { .. }))
        };
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "FLO"));
        let now = Instant::now();
        assert_eq!(windows.window_dragging(1, true, now), vec![]);
        // 51.767: 670x977 asked, and drawn.
        assert_eq!(
            windows.window_resized(1, (670, 977), now),
            vec![live(670, 977)]
        );
        assert!(!resizes(
            &windows.apply(&TaskEvent::Updated(task(1, "FLO", 670, 977)))
        ));
        // 51.927 and 52.065: 676x986 and 677x987 asked; the app draws both.
        assert_eq!(
            windows.window_resized(1, (676, 986), now),
            vec![live(676, 986)]
        );
        assert_eq!(
            windows.window_resized(1, (677, 987), now),
            vec![live(677, 987)]
        );
        for (width, height) in [(676, 986), (677, 987)] {
            assert!(!resizes(
                &windows.apply(&TaskEvent::Updated(task(1, "FLO", width, height)))
            ));
        }
        // The user lets go: the task is reconfigured at the size it was let go at.
        let released = windows.window_dragging(1, false, now);
        assert_eq!(released, vec![end.clone()]);
        // 52.374: a late frame of 670x977 had the window report that size. Android is not
        // asked for it.
        assert_eq!(windows.window_resized(1, (670, 977), now), vec![]);
        // A late report of 670x977 does not resize the window either.
        assert!(!resizes(
            &windows.apply(&TaskEvent::Updated(task(1, "FLO", 670, 977)))
        ));
        assert_eq!(windows.task_size(1), Some((677, 987)));
        // Even had the echo been asked, the drag's end goes out last, with the released size.
        let mut queued = released;
        queued.push(live(670, 977));
        assert_eq!(crate::tasks::coalesce(queued), vec![end]);
        assert!(!resizes(
            &windows.tick(now + super::super::window_sizing::SETTLE * 2)
        ));
    }
}
