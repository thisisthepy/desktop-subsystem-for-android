//! Which window each guest task has, and which window each display's frames go to.
//!
//! The compositor shows one host window per guest app task, and nothing else. This module
//! holds the rules; the platform's compositor carries out the [`WindowChange`]s it returns
//! and asks it where a frame belongs. A frame goes to a window only when the hwcomposer has
//! said which task posts to that display (`TaskFrames`) and that task has a window. Every
//! other frame is dropped, so no window ever shows the guest's whole screen.

use std::collections::{BTreeMap, BTreeSet};
use std::time::Instant;

use super::window_sizing::WindowSizing;
use crate::tasks::{Bounds, Task, TaskEvent, TaskRequest};

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
    /// largest client area the window may have. It changes with every size the window asks
    /// for, without the window itself changing.
    TaskSize {
        task_id: i32,
        width: u32,
        height: u32,
        max: Option<(u32, u32)>,
    },
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

/// How much a frame of `frame` pixels is scaled to fill a view of `view` pixels, in each
/// direction. While the user resizes a window, its last frame is stretched to fill it this
/// way until the guest draws one at the new size.
pub fn fill_scale(frame: (u32, u32), view: (u32, u32)) -> (f32, f32) {
    let scale = |frame: u32, view: u32| {
        if frame == 0 || view == 0 {
            1.0
        } else {
            view as f32 / frame as f32
        }
    };
    (scale(frame.0, view.0), scale(frame.1, view.1))
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
type TaskSize = ((u32, u32), Option<(u32, u32)>);

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
    /// The task size and limit each window was last told.
    told_sizes: BTreeMap<i32, TaskSize>,
}

impl TaskWindows {
    pub fn new() -> TaskWindows {
        TaskWindows::default()
    }

    /// Whether the task has a window now.
    pub fn has_window(&self, task_id: i32) -> bool {
        self.tasks.contains_key(&task_id) && !self.closing.contains(&task_id)
    }

    /// The tasks that have a window, in task ID order.
    pub fn windows(&self) -> Vec<i32> {
        self.tasks
            .keys()
            .copied()
            .filter(|task_id| !self.closing.contains(task_id))
            .collect()
    }

    /// Records a change to the guest's tasks, and returns what the windows must do.
    pub fn apply(&mut self, event: &TaskEvent) -> Vec<WindowChange> {
        match event {
            // An app's name and icon belong to its program, not to a window.
            TaskEvent::App(_) => Vec::new(),
            TaskEvent::Updated(task) => {
                let previous = self.tasks.insert(task.task_id, task.clone());
                let follow = match self.sizes.get_mut(&task.task_id) {
                    Some(sizing) if previous.is_some() => sizing.guest_updated(task),
                    _ => {
                        self.sizes.insert(task.task_id, WindowSizing::new(task));
                        None
                    }
                };
                if self.closing.contains(&task.task_id) {
                    return Vec::new();
                }
                let mut changes = match previous {
                    None => vec![open(task)],
                    Some(previous) => {
                        let mut changes = Vec::new();
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
                        }
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

    /// The request to end a task the user closed did not reach the guest, so the task is
    /// still there: its window opens again.
    pub fn close_failed(&mut self, task_id: i32) -> Vec<WindowChange> {
        if !self.closing.remove(&task_id) {
            return Vec::new();
        }
        let Some(task) = self.tasks.get(&task_id) else {
            return Vec::new();
        };
        self.sizes.insert(task_id, WindowSizing::new(task));
        self.told_sizes.remove(&task_id);
        let mut changes = vec![open(task)];
        changes.extend(self.task_size_change(task_id));
        changes
    }

    /// The change that tells the task's window its task size and limit, when they differ
    /// from what it was last told. A new window was shown at the task's size, so only a
    /// limit is news to it.
    fn task_size_change(&mut self, task_id: i32) -> Option<WindowChange> {
        let now = self.sizes.get(&task_id)?.task_size();
        let told = match self.told_sizes.get(&task_id) {
            Some(told) => *told,
            None => {
                let shown = self
                    .tasks
                    .get(&task_id)
                    .map(|task| window_size(&task.bounds))?;
                (shown, None)
            }
        };
        if told == now {
            return None;
        }
        self.told_sizes.insert(task_id, now);
        let ((width, height), max) = now;
        Some(WindowChange::TaskSize {
            task_id,
            width,
            height,
            max,
        })
    }

    /// The task's window reports the scale of its monitor: the first report is the monitor
    /// it opened on, every later one a move. Returns what the window must do (take a size
    /// that keeps the app the same size to the eye, when it opened on a monitor of another
    /// scale) and the density request.
    pub fn window_dpi(&mut self, task_id: i32, dpi: u32) -> (Vec<WindowChange>, Vec<TaskRequest>) {
        if !self.has_window(task_id) {
            return (Vec::new(), Vec::new());
        }
        let Some(sizing) = self.sizes.get_mut(&task_id) else {
            return (Vec::new(), Vec::new());
        };
        let (size, request) = sizing.dpi_reported(dpi);
        let changes = size
            .map(|(width, height)| WindowChange::Resize {
                task_id,
                width,
                height,
            })
            .into_iter()
            .collect();
        (changes, request.into_iter().collect())
    }

    /// The task's window opened on a monitor of `dpi`. Returns the client size it takes, and
    /// the requests that have the task draw at that monitor's density. `None` when the task
    /// has no window.
    pub fn window_opened(
        &mut self,
        task_id: i32,
        dpi: u32,
    ) -> Option<((u32, u32), Vec<TaskRequest>)> {
        if !self.has_window(task_id) {
            return None;
        }
        let (size, request) = self.sizes.get_mut(&task_id)?.opened(dpi);
        Some((size, request.into_iter().collect()))
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

    /// The task's window is on a monitor of `dpi` now. Returns the request that has the
    /// task draw at that monitor's density.
    pub fn window_dpi_changed(&mut self, task_id: i32, dpi: u32) -> Vec<TaskRequest> {
        if !self.has_window(task_id) {
            return Vec::new();
        }
        self.sizes
            .get_mut(&task_id)
            .and_then(|sizing| sizing.dpi_changed(dpi))
            .into_iter()
            .collect()
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

    /// The largest client area the task's window may have, when the guest said.
    pub fn max_client_size(&self, task_id: i32) -> Option<(u32, u32)> {
        self.sizes.get(&task_id)?.max_client_size()
    }

    /// Time has passed: the windows whose task did not take the size they asked for take the
    /// task's size instead.
    pub fn tick(&mut self, now: Instant) -> Vec<WindowChange> {
        let mut changes = Vec::new();
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
            }
        }
        changes
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

    #[test]
    fn fr3_two_apps_get_two_windows() {
        let mut windows = TaskWindows::new();
        assert_eq!(
            windows.apply(&updated(1, "Clock")),
            vec![opened(1, "Clock")]
        );
        assert_eq!(windows.apply(&updated(2, "Maps")), vec![opened(2, "Maps")]);
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
            windows.apply(&updated(1, "Clock")).len() == 1,
            "a new task 1 opens"
        );
    }

    #[test]
    fn fr3_a_close_that_did_not_reach_the_guest_opens_the_window_again() {
        let mut windows = TaskWindows::new();
        windows.apply(&updated(1, "Clock"));
        windows.user_closed(1);
        assert_eq!(windows.close_failed(1), vec![opened(1, "Clock")]);
        assert!(windows.has_window(1));
        assert!(windows.close_failed(1).is_empty());
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
    fn fr4_a_window_on_another_scale_asks_for_its_monitors_density() {
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 400, 300);
        clock.density_dpi = 160;
        windows.apply(&TaskEvent::Updated(clock));
        assert_eq!(
            windows.window_opened(1, 192),
            Some((
                (800, 600),
                vec![TaskRequest::SetDensity {
                    task_id: 1,
                    density_dpi: 320
                }]
            ))
        );
        assert_eq!(
            windows.window_dpi_changed(1, 96),
            vec![TaskRequest::SetDensity {
                task_id: 1,
                density_dpi: 160
            }]
        );
        // Nothing is asked for a window the user closed.
        windows.user_closed(1);
        assert_eq!(windows.window_dpi_changed(1, 192), vec![]);
        assert_eq!(windows.window_resized(1, (10, 10), Instant::now()), vec![]);
        assert_eq!(windows.window_opened(1, 192), None);
    }

    #[test]
    fn fr4_a_window_hears_its_tasks_size_whenever_the_guest_changes_it() {
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 400, 300);
        // Shown at the task's size, with no limit known: nothing more to say.
        assert_eq!(windows.apply(&TaskEvent::Updated(clock.clone())).len(), 1);
        clock.max_width = 1920;
        clock.max_height = 1080;
        let size = |width, height| WindowChange::TaskSize {
            task_id: 1,
            width,
            height,
            max: Some((1920, 1080)),
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
    fn fr4_the_first_scale_a_window_reports_sizes_it_for_its_monitor() {
        let mut windows = TaskWindows::new();
        let mut clock = task(1, "Clock", 400, 300);
        clock.density_dpi = 160;
        windows.apply(&TaskEvent::Updated(clock));
        assert_eq!(
            windows.window_dpi(1, 144),
            (
                vec![WindowChange::Resize {
                    task_id: 1,
                    width: 600,
                    height: 450
                }],
                vec![TaskRequest::SetDensity {
                    task_id: 1,
                    density_dpi: 240
                }]
            )
        );
        assert_eq!(
            windows.window_dpi(1, 96),
            (
                vec![],
                vec![TaskRequest::SetDensity {
                    task_id: 1,
                    density_dpi: 160
                }]
            )
        );
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

    #[test]
    fn fr3_the_last_frame_is_stretched_to_fill_a_resized_window() {
        assert_eq!(fill_scale((400, 300), (800, 150)), (2.0, 0.5));
        assert_eq!(fill_scale((400, 300), (400, 300)), (1.0, 1.0));
        assert_eq!(fill_scale((0, 300), (800, 0)), (1.0, 1.0));
    }
}
