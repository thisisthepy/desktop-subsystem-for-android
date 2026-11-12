//! Which window each guest task has, and which window each display's frames go to.
//!
//! The compositor shows one host window per guest app task, and nothing else. This module
//! holds the rules; the platform's compositor carries out the [`WindowChange`]s it returns
//! and asks it where a frame belongs. A frame goes to a window only when the hwcomposer has
//! said which task posts to that display (`TaskFrames`) and that task has a window. Every
//! other frame is dropped, so no window ever shows the guest's whole screen.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use super::caption::{caption_tone, CaptionBand, CaptionTone};
use super::window_mode::WindowMode;
use super::window_sizing::{
    to_task_length, to_window_rect, Scale, ScalingMode, WindowSizing, BASE_DENSITY,
    DENSITY_AT_BASE_DPI, SETTLE,
};
use crate::app_windows::AppWindowSettings;
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
    /// The window's caption buttons take this tone, to stay readable over the app's status
    /// bar. Comes with every window that opens, and whenever the app's status bar changes
    /// tone.
    CaptionTone { task_id: i32, tone: CaptionTone },
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
    CaptionMode {
        task_id: i32,
        app_drawn: bool,
        interactive: Vec<Bounds>,
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

/// The tone of the caption glyphs of `task`'s window.
fn tone_of(task: &Task) -> CaptionTone {
    caption_tone(task.status_bar, task.status_bar_color)
}

/// Whether `task`'s app draws its own title bar, and where its controls are in the band. The
/// controls count only while it does.
fn caption_mode_of(task: &Task) -> (bool, Vec<Bounds>) {
    if task.transparent_caption {
        (true, task.caption_interactive.clone())
    } else {
        (false, Vec::new())
    }
}

/// The change that tells `task`'s window whether its app draws its own title bar, with the
/// app's controls in the window's pixels at `scale`.
fn caption_mode_change(task: &Task, scale: Scale) -> WindowChange {
    let (app_drawn, interactive) = caption_mode_of(task);
    WindowChange::CaptionMode {
        task_id: task.task_id,
        app_drawn,
        interactive: interactive
            .into_iter()
            .map(|rect| to_window_rect(rect, scale))
            .collect(),
    }
}

/// What `task`'s window, which shows it at `scale`, must change in its title bar since
/// `previous`, or, for a window that opens (`previous` is `None`), what differs from a
/// window's own title bar showing the icon.
fn caption_changes(previous: Option<&Task>, task: &Task, scale: Scale) -> Vec<WindowChange> {
    let mut changes = Vec::new();
    let mode = caption_mode_of(task);
    let was = previous.map_or((false, Vec::new()), caption_mode_of);
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
    /// Tasks whose window opens once the guest has given them their start size.
    pending: BTreeMap<i32, PendingOpen>,
    /// The mode and resizability each window was last told.
    told_modes: BTreeMap<i32, (WindowMode, bool)>,
    /// Where the guest last said the cursor of each task's focused editor is, in the task's
    /// pixels: shown again at a window's new scale, since the window has it in its own.
    cursors: BTreeMap<i32, crate::ime::ImeCursor>,
    /// The title bar band each window last reported, in its pixels, with the scale its
    /// caption inset was given to the task at: given again at a window's new scale, since the
    /// task has it in its own pixels.
    bands: BTreeMap<i32, (CaptionBand, Scale)>,
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

    /// Whether the task has a window now.
    pub fn has_window(&self, task_id: i32) -> bool {
        self.tasks.contains_key(&task_id)
            && !self.closing.contains(&task_id)
            && !self.pending.contains_key(&task_id)
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

    /// The tasks that have a window, in task ID order.
    pub fn windows(&self) -> Vec<i32> {
        self.tasks
            .keys()
            .copied()
            .filter(|task_id| {
                !self.closing.contains(task_id) && !self.pending.contains_key(task_id)
            })
            .collect()
    }

    /// Takes the user's window settings for every app, which the windows that open from now
    /// on follow. A window open already keeps its size, and its resizability follows at once.
    pub fn set_app_windows(&mut self, apps: &[AppWindowSettings]) -> Vec<WindowChange> {
        self.app_windows = apps
            .iter()
            .map(|app| (app.package_name.clone(), app.clone()))
            .collect();
        let open = self.windows();
        open.into_iter()
            .filter_map(|task_id| self.mode_change(task_id, false))
            .collect()
    }

    /// How `task`'s window follows a monitor of another scale: as the user chose for its app,
    /// and as its top activity says it handles a change of density.
    fn scaling_of(&self, task: &Task) -> ScalingMode {
        self.app_windows
            .get(&task.package_name)
            .map(|app| app.dpi_scaling)
            .unwrap_or_default()
            .mode(task.handles_density_change)
    }

    /// A new record of how `task`'s window follows its task.
    fn sizing_for(&self, task: &Task) -> WindowSizing {
        WindowSizing::with_mode(task, self.scaling_of(task))
    }

    /// The scale `task_id`'s window shows its task at.
    fn scale_of(&self, task_id: i32) -> Scale {
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
            WindowMode::of(task.fullscreen, task.picture_in_picture),
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
        match event {
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
                        sizing.guest_updated(task)
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
                    self.displays.remove(task_id);
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
        if !self.has_window(task_id) {
            return None;
        }
        self.closing.insert(task_id);
        Some(TaskRequest::Close { task_id })
    }

    /// The task's window went away without the user closing it (its window program crashed),
    /// and the task goes on: its window opens again, as it first opened, with everything the
    /// new window has to be told. Nothing for a task that has no window.
    pub fn reopen(&mut self, task_id: i32) -> Vec<WindowChange> {
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
    /// A task whose app does not say it handles a change of density is not asked for another
    /// unless the user chose the app mode for it (Android then recreates its activity): its
    /// window shows it scaled to the monitor instead, and hears the scale.
    pub fn window_dpi(&mut self, task_id: i32, dpi: u32) -> (Vec<WindowChange>, Vec<TaskRequest>) {
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
        if !self.has_window(task_id) {
            return Vec::new();
        }
        let scale = self.scale_of(task_id);
        self.bands.insert(task_id, (band, scale));
        vec![caption_request(task_id, band, scale)]
    }

    /// The task's window opened on a monitor of `dpi`. Returns the client size it takes, the
    /// requests that have the task draw at that monitor's density, and what else the window
    /// must hear: the scale it shows a task at that keeps its density. `None` when the task
    /// has no window.
    pub fn window_opened(&mut self, task_id: i32, dpi: u32) -> Option<WindowOpened> {
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
        self.sizes
            .get_mut(&task_id)
            .and_then(|sizing| sizing.view_resized(view, now))
            .into_iter()
            .collect()
    }

    /// The task's window is on a monitor of `dpi` now. Returns what the window must hear (the
    /// scale it shows a task at that keeps its density) and the request that has a task that
    /// follows its monitor draw at that monitor's density.
    pub fn window_dpi_changed(
        &mut self,
        task_id: i32,
        dpi: u32,
    ) -> (Vec<WindowChange>, Vec<TaskRequest>) {
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
    pub fn window_dragging(
        &mut self,
        task_id: i32,
        active: bool,
        now: Instant,
    ) -> Vec<TaskRequest> {
        if !self.has_window(task_id) {
            return Vec::new();
        }
        self.sizes
            .get_mut(&task_id)
            .and_then(|sizing| sizing.dragging(active, now))
            .into_iter()
            .collect()
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
        for (task_id, sizing) in &mut self.sizes {
            if self.closing.contains(task_id) || !self.tasks.contains_key(task_id) {
                continue;
            }
            if let Some((width, height)) = sizing.tick(now) {
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
        changes
    }

    /// The requests that reconfigure each task for a size its window gave it live outside a
    /// drag (a maximise, a snap), now that the task has taken it, and that give the caption
    /// inset again, in the task's pixels, to a task whose window shows it at another scale
    /// than when its band was last given. Asked after every change to the tasks and at every
    /// tick.
    pub fn reconfigures(&mut self, now: Instant) -> Vec<TaskRequest> {
        let banded: Vec<i32> = self.bands.keys().copied().collect();
        let mut requests: Vec<TaskRequest> = banded
            .into_iter()
            .filter_map(|task_id| self.recaption(task_id))
            .collect();
        requests.extend(self.size_reconfigures(now));
        requests
    }

    /// The caption inset again, when `task_id`'s window shows its task at another scale than
    /// the one its band was last given at.
    fn recaption(&mut self, task_id: i32) -> Option<TaskRequest> {
        if !self.has_window(task_id) {
            return None;
        }
        let scale = self.scale_of(task_id);
        let (band, given) = self.bands.get_mut(&task_id)?;
        if *given == scale {
            return None;
        }
        *given = scale;
        Some(caption_request(task_id, *band, scale))
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

    fn dark_glyphs(task_id: i32) -> WindowChange {
        WindowChange::CaptionTone {
            task_id,
            tone: CaptionTone::DarkGlyphs,
        }
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
                app_drawn: true,
                interactive: vec![wider],
            }));
        // Withdrawn, the window takes its own title bar back, and the controls no longer count.
        drawing.transparent_caption = false;
        assert!(windows
            .apply(&TaskEvent::Updated(drawing))
            .contains(&WindowChange::CaptionMode {
                task_id: 1,
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
                    tone: CaptionTone::LightGlyphs
                }
            ]
        );
        assert!(windows.apply(&TaskEvent::Updated(dark.clone())).is_empty());
        // The app switches to a light theme.
        let mut light = dark.clone();
        light.status_bar = StatusBarAppearance::Light;
        assert_eq!(
            windows.apply(&TaskEvent::Updated(light.clone())),
            vec![dark_glyphs(1)]
        );
        // A new color with the same tone changes nothing.
        light.status_bar_color = 0xFF00_0000;
        assert!(windows.apply(&TaskEvent::Updated(light)).is_empty());
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
        assert_eq!(
            windows.window_dpi_changed(1, 96),
            (
                vec![],
                vec![TaskRequest::SetDensity {
                    task_id: 1,
                    density_dpi: 130
                }]
            )
        );
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
        assert_eq!(
            windows.window_dpi(1, 96),
            (
                vec![],
                vec![TaskRequest::SetDensity {
                    task_id: 1,
                    density_dpi: 130
                }]
            )
        );
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
        assert_eq!(
            requests,
            vec![TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 262
            }]
        );
        assert_eq!(windows.scale(1), (1, 1));
        // The desktop doubles the window, and the task takes the window's size.
        assert_eq!(
            windows.window_resized(1, (800, 600), now),
            vec![TaskRequest::Resize {
                task_id: 1,
                width: 800,
                height: 600,
                live: true,
            }]
        );
        // Another activity on top that does not say so: the task keeps the density it has
        // from now on, and a later move scales the window instead.
        notes.density_dpi = 262;
        notes.bounds.right = notes.bounds.left + 800;
        notes.bounds.bottom = notes.bounds.top + 600;
        notes.handles_density_change = false;
        windows.apply(&TaskEvent::Updated(notes));
        let (_, requests) = windows.window_dpi(1, 96);
        assert!(!asks_for_density(&requests));
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
        let (_, requests) = windows.window_dpi(1, 192);
        assert_eq!(
            requests,
            vec![TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 262
            }]
        );
        assert_eq!(windows.scale(1), (1, 1));
        // Its next activity does not say so either, and the choice still holds.
        notes.density_dpi = 262;
        windows.apply(&TaskEvent::Updated(notes));
        let (_, requests) = windows.window_dpi(1, 96);
        assert_eq!(
            requests,
            vec![TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 130
            }]
        );
        assert_eq!(windows.scale(1), (1, 1));

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
        // 400 x 300 dp at 320 dpi.
        assert_eq!(
            windows.reconfigures(Instant::now()),
            vec![TaskRequest::Resize {
                task_id: 1,
                width: 800,
                height: 600,
                live: false,
            }]
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
    fn fr30_full_screen_and_picture_in_picture_requests_reach_the_window() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Video"));
        let full = Task {
            fullscreen: true,
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
}
