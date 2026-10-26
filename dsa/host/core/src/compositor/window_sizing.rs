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

/// Where a maximised window of `outer` pixels goes, along one direction, on a work area
/// that starts at `start` and is `area` pixels long, when the window manager proposed
/// `proposed`. A window is never larger than the guest can draw, so on a monitor larger
/// than that it maximises to the guest's largest size, shown 1:1, and is centred on the work
/// area rather than left in its corner. A window that fills the work area stays where the
/// window manager put it.
pub fn maximized_position(proposed: i32, start: i32, area: i32, outer: i32) -> i32 {
    if outer >= area {
        return proposed;
    }
    start + (area - outer) / 2
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
    /// Whether the task was given a size live during this drag, so it must be reconfigured
    /// when the drag ends.
    resized_live: bool,
    /// Whether the task was given a size live outside a drag (a maximise, a snap) and still
    /// has to be reconfigured for it once it has taken it.
    reconfigure: bool,
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
            resized_live: false,
            reconfigure: false,
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
    ///
    /// The request is always live: the task takes the size without being reconfigured, so an
    /// app that does not handle a change of screen size is laid out again within a frame
    /// rather than recreated at the new size. Recreating an app can take longer than the
    /// guest holds a resized task's frames, and a maximise asked for at once then showed the
    /// old, small picture on black before the app had drawn again.
    ///
    /// The task is reconfigured afterwards, at a size it already has: at the end of the drag
    /// ([`dragging`](Self::dragging)), or, for a change of size outside a drag (a maximise,
    /// a snap, a move to a monitor of another scale), once the guest has taken the size
    /// ([`reconfigure`](Self::reconfigure)).
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
        if self.dragging {
            self.resized_live = true;
        } else {
            self.reconfigure = true;
        }
        Some(TaskRequest::Resize {
            task_id: self.task_id,
            width: size.0,
            height: size.1,
            live: true,
        })
    }

    /// Returns the request (not live) that reconfigures the task for a size it was given
    /// live outside a drag, once the guest has taken that size, or once it has had
    /// [`SETTLE`] to and the window has gone back to the size the task has
    /// ([`tick`](Self::tick)). The task has that size already, so it is recreated in place
    /// and the window waits for nothing. `None` while there is nothing to reconfigure, while
    /// the guest is still on its way, and while the user drags the window, whose end
    /// reconfigures the task instead.
    pub fn reconfigure(&mut self, now: Instant) -> Option<TaskRequest> {
        if !self.reconfigure || self.dragging {
            return None;
        }
        if let Some((_, since)) = self.asked {
            if now.saturating_duration_since(since) < SETTLE {
                return None;
            }
        }
        self.reconfigure = false;
        Some(TaskRequest::Resize {
            task_id: self.task_id,
            width: self.guest.0,
            height: self.guest.1,
            live: false,
        })
    }

    /// The user started (`true`) or finished dragging the window. While they drag, the
    /// window never resizes itself under them. The guest has [`SETTLE`] from the end of the
    /// drag to take the last size.
    ///
    /// When a drag that resized the task live ends, returns the request (not live) that
    /// gives the task the window's size and reconfigures it for that size. It is sent even
    /// when the task has that size already: the live steps left the task with the
    /// configuration it had when the drag began. A drag that only moved the window asks
    /// nothing.
    pub fn dragging(&mut self, active: bool, now: Instant) -> Option<TaskRequest> {
        if active {
            if !self.dragging {
                self.dragging = true;
                // A size given live just before the drag is reconfigured with the drag's.
                self.resized_live = std::mem::take(&mut self.reconfigure);
            }
            return None;
        }
        if !self.dragging {
            return None;
        }
        self.dragging = false;
        if !std::mem::take(&mut self.resized_live) {
            if let Some((size, _)) = self.asked {
                self.asked = Some((size, now));
            }
            return None;
        }
        let size = clamp_size(self.view, self.max);
        // A task that has the size already reports the same bounds after it is
        // reconfigured, if it reports at all, so there is nothing to wait for.
        self.asked = (size != self.guest).then_some((size, now));
        Some(TaskRequest::Resize {
            task_id: self.task_id,
            width: size.0,
            height: size.1,
            live: false,
        })
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

    fn live_resize(width: u32, height: u32) -> Option<TaskRequest> {
        Some(TaskRequest::Resize {
            task_id: 7,
            width,
            height,
            live: true,
        })
    }

    fn density(density_dpi: u32) -> Option<TaskRequest> {
        Some(TaskRequest::SetDensity {
            task_id: 7,
            density_dpi,
        })
    }

    #[test]
    fn fr4_sizes_asked_while_dragging_are_live_and_the_drag_end_reconfigures_once() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        let now = Instant::now();
        assert_eq!(sizing.dragging(true, now), None);
        assert_eq!(sizing.view_resized((500, 300), now), live_resize(500, 300));
        assert_eq!(sizing.view_resized((600, 300), now), live_resize(600, 300));
        // The guest takes each live size and reports it as it would any other.
        assert_eq!(sizing.guest_updated(&task(500, 300, 160)), None);
        assert_eq!(sizing.guest_updated(&task(600, 300, 160)), None);
        assert!(!sizing.waiting());
        // The task has the size already; the end of the drag asks for it again, not live,
        // which is what reconfigures it, and waits for nothing.
        assert_eq!(sizing.dragging(false, now), resize(600, 300));
        assert!(!sizing.waiting());
        // The guest's report of it carries the same bounds and changes nothing.
        assert_eq!(sizing.guest_updated(&task(600, 300, 160)), None);
        assert_eq!(sizing.tick(now + SETTLE * 2), None);
        // A change of size outside a drag (a maximise, a snap) is live too.
        assert_eq!(sizing.view_resized((900, 700), now), live_resize(900, 700));
    }

    #[test]
    fn fr4_a_maximise_gives_the_task_its_size_live_then_reconfigures_it_at_that_size() {
        // A 608x1080 task on a monitor at 200%; the guest draws at most 1920x1080.
        let mut sizing = WindowSizing::new(&task(608, 1080, 320));
        let now = Instant::now();
        assert_eq!(sizing.reconfigure(now), None, "nothing to reconfigure");
        // The window is maximised to the most the guest can draw: no drag, one size.
        assert_eq!(
            sizing.view_resized((1920, 1080), now),
            live_resize(1920, 1080)
        );
        // Not before the guest has taken the size: the app would be recreated at a size it
        // has not drawn at, and the window would show the old picture on black meanwhile.
        assert_eq!(sizing.reconfigure(now), None);
        assert_eq!(sizing.guest_updated(&task(1920, 1080, 320)), None);
        // Then once, at the size the task has, which the window keeps.
        assert_eq!(sizing.reconfigure(now), resize(1920, 1080));
        assert_eq!(sizing.reconfigure(now), None, "asked once");
        assert_eq!(sizing.guest_updated(&task(1920, 1080, 320)), None);
        assert_eq!(sizing.tick(now + SETTLE * 2), None);
        // Restoring the window goes the same way.
        assert_eq!(
            sizing.view_resized((608, 1080), now),
            live_resize(608, 1080)
        );
        assert_eq!(sizing.guest_updated(&task(608, 1080, 320)), None);
        assert_eq!(sizing.reconfigure(now), resize(608, 1080));
    }

    #[test]
    fn fr4_a_maximise_on_a_monitor_larger_than_the_guest_asks_for_no_more_than_it_draws() {
        // A 4K monitor's work area at 200% is larger than the 1920x1080 the guest draws.
        let mut sizing = WindowSizing::new(&task(608, 1080, 320));
        assert_eq!(
            sizing.view_resized((3840, 2054), Instant::now()),
            live_resize(1920, 1080)
        );
        assert_eq!(sizing.max_client_size(), Some((1920, 1080)));
    }

    #[test]
    fn fr4_a_size_the_guest_never_takes_still_ends_with_the_task_reconfigured() {
        let mut sizing = WindowSizing::new(&task(608, 1080, 320));
        let start = Instant::now();
        sizing.view_resized((1920, 1080), start);
        assert_eq!(sizing.reconfigure(start + SETTLE / 2), None);
        // The window goes back to the task's size, and the task is reconfigured at it, so
        // it does not keep the configuration the live size held.
        assert_eq!(sizing.tick(start + SETTLE), Some((608, 1080)));
        assert_eq!(sizing.reconfigure(start + SETTLE), resize(608, 1080));
    }

    #[test]
    fn fr4_a_drag_that_starts_before_a_maximise_is_reconfigured_reconfigures_it_once() {
        let mut sizing = WindowSizing::new(&task(608, 1080, 320));
        let now = Instant::now();
        sizing.view_resized((1920, 1080), now);
        sizing.guest_updated(&task(1920, 1080, 320));
        // The user takes hold of the window before the reconfigure went out.
        assert_eq!(sizing.dragging(true, now), None);
        assert_eq!(sizing.reconfigure(now), None, "not under a drag");
        // A drag that only moves it still ends with the task reconfigured, once.
        assert_eq!(sizing.dragging(false, now), resize(1920, 1080));
        assert_eq!(sizing.reconfigure(now), None);
    }

    #[test]
    fn fr4_a_window_maximised_smaller_than_the_work_area_is_centred_on_it() {
        // A 1920x1080 client (1936x1119 outer at 100%) on a 3840x2100 work area at (0, 0).
        assert_eq!(maximized_position(-8, 0, 3840, 1936), 952);
        assert_eq!(maximized_position(-8, 0, 2100, 1119), 490);
        // On a second monitor, from its own corner.
        assert_eq!(maximized_position(2552, 2560, 3840, 1936), 2560 + 952);
        // A window as large as the work area, borders and all, stays where it was put.
        assert_eq!(maximized_position(-8, 0, 1920, 1936), -8);
        assert_eq!(maximized_position(-8, 0, 1920, 1920), -8);
    }

    #[test]
    fn fr4_the_drag_end_asks_for_a_size_the_guest_has_not_reported_yet() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        let start = Instant::now();
        sizing.dragging(true, start);
        assert_eq!(
            sizing.view_resized((700, 300), start),
            live_resize(700, 300)
        );
        let end = start + SETTLE / 4;
        assert_eq!(sizing.dragging(false, end), resize(700, 300));
        assert!(sizing.waiting());
        // The answer to either request clears the wait.
        assert_eq!(sizing.guest_updated(&task(700, 300, 160)), None);
        assert!(!sizing.waiting());
        assert_eq!(sizing.tick(end + SETTLE * 2), None);
    }

    #[test]
    fn fr4_the_drag_end_asks_for_no_more_than_the_guest_can_draw() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        let now = Instant::now();
        sizing.dragging(true, now);
        assert_eq!(
            sizing.view_resized((2560, 900), now),
            live_resize(1920, 900)
        );
        assert_eq!(sizing.dragging(false, now), resize(1920, 900));
    }

    #[test]
    fn fr4_a_drag_that_does_not_resize_the_task_asks_nothing_at_its_end() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        let now = Instant::now();
        // The user only moves the window.
        assert_eq!(sizing.dragging(true, now), None);
        assert_eq!(sizing.dragging(false, now), None);
        // A size the task has already is not a resize either.
        sizing.dragging(true, now);
        assert_eq!(sizing.view_resized((400, 300), now), None);
        assert_eq!(sizing.dragging(false, now), None);
        // An end the window reports twice asks once.
        sizing.dragging(true, now);
        sizing.view_resized((500, 300), now);
        assert_eq!(sizing.dragging(false, now), resize(500, 300));
        assert_eq!(sizing.dragging(false, now), None);
    }

    #[test]
    fn fr4_a_start_the_window_reports_twice_does_not_forget_the_live_sizes() {
        let mut sizing = WindowSizing::new(&task(400, 300, 160));
        let now = Instant::now();
        sizing.dragging(true, now);
        sizing.view_resized((500, 300), now);
        assert_eq!(sizing.dragging(true, now), None);
        assert_eq!(sizing.dragging(false, now), resize(500, 300));
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
        assert_eq!(sizing.view_resized((640, 480), now), live_resize(640, 480));
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
            live_resize(1920, 900)
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
        assert_eq!(sizing.dragging(false, now), resize(600, 300));
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
        assert_eq!(sizing.dragging(false, end), resize(700, 300));
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
        assert_eq!(
            sizing.view_resized((1096, 1948), now),
            live_resize(1096, 1080)
        );
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
            live_resize(800, 600)
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
