//! How a task window's size and scale follow the user, and how its task follows the window.
//!
//! One guest pixel is one pixel of the window. A task's frames are shown 1:1, so they are as
//! sharp on a monitor at 200% as at 100%. What keeps an app the same size to the eye across
//! monitors is its density: the task draws at [`density_for_dpi`] of its window's monitor,
//! so a task that moves from a 100% to a 200% monitor draws at 320 dpi into twice the
//! pixels, and its text stays sharp instead of being scaled up.
//!
//! The guest has one display, but density is set per task, so two windows on monitors of
//! different scales each get the density of their own monitor.
//!
//! [`WindowSizing`] holds one window's side of this. The platform tells it what the user and
//! the window manager did (a new client size, a new monitor scale, a drag starting or
//! ending) and carries out what it returns: a request to the guest, or a size the window
//! must take. It names no window system, so a window process of any platform can use it.

use std::time::{Duration, Instant};

use super::task_windows::{fit_within, window_size};
use crate::tasks::{Task, TaskRequest};

/// A monitor's dots per inch at a scale of 100%.
pub const BASE_DPI: u32 = 96;

/// The density at which an app draws one pixel per density-independent pixel.
pub const BASE_DENSITY: u32 = 160;

/// How long the guest has to take a size the host asked for before the window follows the
/// size the guest did take instead. The agent reports the size it is giving a task as soon
/// as it asks for it, so the answer normally arrives in a few milliseconds; this is for a
/// guest that cannot give the size at all.
pub const SETTLE: Duration = Duration::from_secs(2);

/// The density a task draws at on a monitor of `dpi`, so it looks the size it would at 100%:
/// 160 at 96 dpi (100%), 240 at 144 dpi (150%), 320 at 192 dpi (200%). A monitor that
/// reports no DPI counts as 100%.
pub fn density_for_dpi(dpi: u32) -> u32 {
    let dpi = if dpi == 0 { BASE_DPI } else { dpi };
    let density =
        (u64::from(dpi) * u64::from(BASE_DENSITY) + u64::from(BASE_DPI) / 2) / u64::from(BASE_DPI);
    density.min(u64::from(u32::MAX)) as u32
}

/// `size`, drawn at density `from`, rescaled to density `to`: the same size in
/// density-independent pixels. Unchanged when either density is unknown (0).
pub fn rescale(size: (u32, u32), from: u32, to: u32) -> (u32, u32) {
    if from == 0 || to == 0 || from == to {
        return size;
    }
    let scale = |length: u32| {
        let scaled = (u64::from(length) * u64::from(to) + u64::from(from) / 2) / u64::from(from);
        scaled.clamp(1, u64::from(u32::MAX)) as u32
    };
    (scale(size.0), scale(size.1))
}

/// `size` cut down to `max` in each direction, when there is a limit.
pub fn clamp_size(size: (u32, u32), max: Option<(u32, u32)>) -> (u32, u32) {
    match max {
        Some((width, height)) => (size.0.min(width.max(1)), size.1.min(height.max(1))),
        None => size,
    }
}

/// One task window's size and density, as the host and the guest each see them.
#[derive(Debug, Clone)]
pub struct WindowSizing {
    task_id: i32,
    /// The task's size as the guest last reported it.
    guest: (u32, u32),
    /// The density the guest last reported for the task; 0 when unknown.
    density: u32,
    /// The largest size the guest can give the task.
    max: Option<(u32, u32)>,
    /// The window's client size.
    view: (u32, u32),
    /// The size last asked of the guest that it has not yet reported, and when.
    asked: Option<((u32, u32), Instant)>,
    /// The density last asked of the guest.
    asked_density: Option<u32>,
    /// Whether the user is dragging the window's edge or moving it.
    dragging: bool,
    /// Whether the window has said which monitor scale it opened on.
    placed: bool,
}

impl WindowSizing {
    /// A window that shows `task` at the size the guest gave it.
    pub fn new(task: &Task) -> WindowSizing {
        let guest = window_size(&task.bounds);
        WindowSizing {
            task_id: task.task_id,
            guest,
            density: task.density_dpi,
            max: task.max_size(),
            view: guest,
            asked: None,
            asked_density: None,
            dragging: false,
            placed: false,
        }
    }

    /// The largest client area the window may have: what the guest can draw. A larger window
    /// could only show the task's frames stretched.
    pub fn max_client_size(&self) -> Option<(u32, u32)> {
        self.max
    }

    /// The task's size as the guest last reported it.
    pub fn guest_size(&self) -> (u32, u32) {
        self.guest
    }

    /// Whether the window is waiting for the guest to take a size it asked for.
    pub fn waiting(&self) -> bool {
        self.asked.is_some()
    }

    /// The window opened on a monitor of `dpi`. Returns the client size it opens at (the
    /// task's size rescaled to the monitor's density, within what the guest can draw) and
    /// the density request, if the task draws at another density. The window then reports
    /// its client size through [`view_resized`](Self::view_resized), which asks the guest
    /// for that size.
    pub fn opened(&mut self, dpi: u32) -> ((u32, u32), Option<TaskRequest>) {
        let request = self.dpi_changed(dpi);
        let size = rescale(self.guest, self.density, density_for_dpi(dpi));
        let size = match self.max {
            Some(max) => fit_within(size, max),
            None => size,
        };
        (size, request)
    }

    /// The window reports the scale of its monitor. The first report is the monitor it
    /// opened on ([`opened`](Self::opened)): it returns the client size the window takes,
    /// when that differs from the size it has, and the density request. Every later report
    /// is a move to another monitor ([`dpi_changed`](Self::dpi_changed)).
    pub fn dpi_reported(&mut self, dpi: u32) -> (Option<(u32, u32)>, Option<TaskRequest>) {
        if self.placed {
            return (None, self.dpi_changed(dpi));
        }
        self.placed = true;
        let (size, request) = self.opened(dpi);
        if size == self.view {
            return (None, request);
        }
        self.view = size;
        (Some(size), request)
    }

    /// The task's size as the guest has it and the largest client area the window may have,
    /// which the window measures its pointer input in and keeps to.
    pub fn task_size(&self) -> ((u32, u32), Option<(u32, u32)>) {
        (self.guest, self.max)
    }

    /// The window is now on a monitor of `dpi`. Returns the request that has the task draw
    /// at that monitor's density, unless it does already. The window manager resizes the
    /// window to keep its size to the eye, and that new size follows through
    /// [`view_resized`](Self::view_resized), so the guest gets the density first and the
    /// size right after it.
    pub fn dpi_changed(&mut self, dpi: u32) -> Option<TaskRequest> {
        let wanted = density_for_dpi(dpi);
        if self.asked_density.unwrap_or(self.density) == wanted {
            return None;
        }
        self.asked_density = Some(wanted);
        Some(TaskRequest::SetDensity {
            task_id: self.task_id,
            density_dpi: wanted,
        })
    }

    /// The window's client area is now `view`. Returns the request that gives the task that
    /// size, unless the task has it or it was asked for already. Called for every step of a
    /// live resize; the requests are folded together before they are sent.
    pub fn view_resized(&mut self, view: (u32, u32), now: Instant) -> Option<TaskRequest> {
        if view.0 == 0 || view.1 == 0 {
            return None;
        }
        self.view = view;
        let size = clamp_size(view, self.max);
        let current = self.asked.map(|(size, _)| size).unwrap_or(self.guest);
        if size == current {
            return None;
        }
        self.asked = Some((size, now));
        Some(TaskRequest::Resize {
            task_id: self.task_id,
            width: size.0,
            height: size.1,
            live: false,
        })
    }

    /// The user started (`true`) or finished dragging the window. While they drag, the
    /// window never resizes itself under them. The guest has [`SETTLE`] from the end of the
    /// drag to take the last size.
    pub fn dragging(&mut self, active: bool, now: Instant) {
        self.dragging = active;
        if !active {
            if let Some((size, _)) = self.asked {
                self.asked = Some((size, now));
            }
        }
    }

    /// The guest reported the task. Returns the client size the window must take, when the
    /// guest resized the task on its own; `None` when the window keeps its size, as it does
    /// while the guest catches up with sizes the host asked for.
    pub fn guest_updated(&mut self, task: &Task) -> Option<(u32, u32)> {
        let size = window_size(&task.bounds);
        self.density = task.density_dpi;
        if let Some(max) = task.max_size() {
            self.max = Some(max);
        }
        let previous = std::mem::replace(&mut self.guest, size);
        match self.asked {
            Some((asked, _)) if asked == size => {
                self.asked = None;
                None
            }
            // Still on its way to the size last asked for.
            Some(_) => None,
            None if size != previous && size != self.view && !self.dragging => {
                self.view = size;
                Some(size)
            }
            None => None,
        }
    }

    /// Time has passed. Returns the client size the window must take when the guest has not
    /// taken the size the host asked for within [`SETTLE`]: the window then shows the task
    /// at the size it has, 1:1, rather than its frames stretched for good.
    pub fn tick(&mut self, now: Instant) -> Option<(u32, u32)> {
        let (_, since) = self.asked?;
        if self.dragging || now.saturating_duration_since(since) < SETTLE {
            return None;
        }
        self.asked = None;
        if self.guest == self.view {
            return None;
        }
        self.view = self.guest;
        Some(self.guest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::Bounds;

    fn task(width: i32, height: i32, density_dpi: u32) -> Task {
        Task {
            task_id: 7,
            bounds: Bounds {
                left: 30,
                top: 40,
                right: 30 + width,
                bottom: 40 + height,
            },
            package_name: "com.example.notes".into(),
            density_dpi,
            max_width: 1920,
            max_height: 1080,
            ..Task::default()
        }
    }

    fn resize(width: u32, height: u32) -> Option<TaskRequest> {
        Some(TaskRequest::Resize {
            task_id: 7,
            width,
            height,
            live: false,
        })
    }

    fn density(density_dpi: u32) -> Option<TaskRequest> {
        Some(TaskRequest::SetDensity {
            task_id: 7,
            density_dpi,
        })
    }

    #[test]
    fn fr4_density_follows_the_monitor_scale() {
        assert_eq!(density_for_dpi(96), 160);
        assert_eq!(density_for_dpi(120), 200);
        assert_eq!(density_for_dpi(144), 240);
        assert_eq!(density_for_dpi(168), 280);
        assert_eq!(density_for_dpi(192), 320);
        assert_eq!(density_for_dpi(288), 480);
        assert_eq!(
            density_for_dpi(0),
            160,
            "a monitor that reports nothing is 100%"
        );
    }

    #[test]
    fn fr4_a_size_keeps_its_density_independent_size_across_densities() {
        assert_eq!(rescale((548, 974), 160, 320), (1096, 1948));
        assert_eq!(rescale((1096, 1948), 320, 160), (548, 974));
        assert_eq!(rescale((548, 974), 160, 240), (822, 1461));
        assert_eq!(rescale((548, 974), 0, 320), (548, 974));
        assert_eq!(
            rescale((1, 1), 320, 160),
            (1, 1),
            "never shrinks to nothing"
        );
    }

    #[test]
    fn fr4_resizing_the_window_asks_the_guest_for_its_client_size() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        let now = Instant::now();
        assert_eq!(sizing.view_resized((640, 480), now), resize(640, 480));
        // The same size again, or the size the task already has, asks nothing.
        assert_eq!(sizing.view_resized((640, 480), now), None);
        let mut fresh = WindowSizing::new(&task(400, 300, 160));
        assert_eq!(fresh.view_resized((400, 300), now), None);
        assert_eq!(
            fresh.view_resized((0, 300), now),
            None,
            "a minimised window"
        );
    }

    #[test]
    fn fr4_the_guest_is_never_asked_for_more_than_it_can_draw() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        assert_eq!(sizing.max_client_size(), Some((1920, 1080)));
        assert_eq!(
            sizing.view_resized((2560, 900), Instant::now()),
            resize(1920, 900)
        );
    }

    #[test]
    fn fr4_the_window_does_not_jump_while_the_guest_catches_up() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        let now = Instant::now();
        sizing.dragging(true, now);
        sizing.view_resized((500, 300), now);
        sizing.view_resized((600, 300), now);
        // The guest reports the sizes on the way; the window keeps the user's size.
        assert_eq!(sizing.guest_updated(&task(500, 300, 160)), None);
        assert!(sizing.waiting());
        assert_eq!(sizing.guest_updated(&task(600, 300, 160)), None);
        assert!(!sizing.waiting());
        sizing.dragging(false, now);
        assert_eq!(sizing.tick(now + SETTLE * 2), None);
    }

    #[test]
    fn fr4_a_size_the_guest_does_not_take_is_not_left_stretched() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        let start = Instant::now();
        sizing.view_resized((700, 300), start);
        // The guest gives the task another size than the one asked for.
        assert_eq!(sizing.guest_updated(&task(650, 300, 160)), None);
        assert_eq!(sizing.tick(start + SETTLE / 2), None, "it may still come");
        assert_eq!(sizing.tick(start + SETTLE), Some((650, 300)));
        // Once the window has taken it, nothing more is asked.
        assert_eq!(sizing.view_resized((650, 300), start + SETTLE), None);
        assert_eq!(sizing.tick(start + SETTLE * 3), None);
    }

    #[test]
    fn fr4_the_window_never_resizes_itself_under_a_drag() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        let start = Instant::now();
        sizing.dragging(true, start);
        sizing.view_resized((700, 300), start);
        assert_eq!(sizing.guest_updated(&task(650, 300, 160)), None);
        assert_eq!(sizing.tick(start + SETTLE * 5), None);
        // The guest has SETTLE from the end of the drag.
        let end = start + SETTLE * 5;
        sizing.dragging(false, end);
        assert_eq!(sizing.tick(end + SETTLE / 2), None);
        assert_eq!(sizing.tick(end + SETTLE), Some((650, 300)));
    }

    #[test]
    fn fr4_a_task_the_guest_resizes_on_its_own_resizes_its_window() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        assert_eq!(sizing.guest_updated(&task(800, 600, 160)), Some((800, 600)));
        // The window reporting that size asks nothing back.
        assert_eq!(sizing.view_resized((800, 600), Instant::now()), None);
        // A task that only moves leaves its window alone.
        let mut moved = task(800, 600, 160);
        moved.bounds.left += 100;
        moved.bounds.right += 100;
        assert_eq!(sizing.guest_updated(&moved), None);
    }

    #[test]
    fn fr4_moving_to_a_monitor_of_another_scale_asks_for_its_density_first() {
        let mut sizing = WindowSizing::new(&task(548, 974, 160));
        let now = Instant::now();
        assert_eq!(sizing.dpi_changed(96), None, "already right at 100%");
        assert_eq!(sizing.dpi_changed(192), density(320));
        assert_eq!(sizing.dpi_changed(192), None, "asked once");
        // The window manager doubles the window; the size follows the density.
        assert_eq!(sizing.view_resized((1096, 1948), now), resize(1096, 1080));
        // And back.
        assert_eq!(sizing.dpi_changed(96), density(160));
    }

    #[test]
    fn fr4_the_first_scale_a_window_reports_is_where_it_opened() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        assert_eq!(sizing.dpi_reported(192), (Some((800, 600)), density(320)));
        // The window reports its new size; the task is asked for it.
        assert_eq!(
            sizing.view_resized((800, 600), Instant::now()),
            resize(800, 600)
        );
        // A later report is a move.
        assert_eq!(sizing.dpi_reported(96), (None, density(160)));
        let mut at_100 = WindowSizing::new(&task(400, 300, 160));
        assert_eq!(at_100.dpi_reported(96), (None, None));
        assert_eq!(at_100.task_size(), ((400, 300), Some((1920, 1080))));
    }

    #[test]
    fn fr4_a_window_opens_at_its_monitors_scale() {
        let mut on_200 = WindowSizing::new(&task(548, 974, 160));
        assert_eq!(on_200.opened(192), ((608, 1080), density(320)));
        let mut on_100 = WindowSizing::new(&task(548, 974, 160));
        assert_eq!(on_100.opened(96), ((548, 974), None));
        let mut on_150 = WindowSizing::new(&task(400, 300, 160));
        assert_eq!(on_150.opened(144), ((600, 450), density(240)));
        // A task whose density the agent could not read keeps its size.
        let mut unknown = WindowSizing::new(&task(400, 300, 0));
        assert_eq!(unknown.opened(192), ((400, 300), density(320)));
    }
}
