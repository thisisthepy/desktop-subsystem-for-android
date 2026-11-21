//! Closing a window that outlived a window manager restart but only ever shows an empty frame.
//!
//! The windows of an app stay open when the window manager crashes or restarts, and the new
//! window manager shows the tasks in them again. The graphics streams of the apps in Android
//! ended with the old renderer, though, and the new renderer does not know the buffers an
//! app drew into before: such an app's frames reach the window as one colour, and stay so.
//! Left alone the window is black for good while its task still runs.
//!
//! [`BlankWatch`] follows the tasks that were there when the window manager started
//! ([`BlankWatch::inherit`]) and whether the newest frame of each is one colour. A task whose
//! frame is not one colour has recovered and is let go; one that stays one colour for
//! [`BLANK_AFTER`] is returned by [`BlankWatch::due`], and its window is closed as when the
//! user closes it, so that opening the app again gives a new task with new buffers instead of
//! leaving a window that shows nothing. Tasks that start after the window manager are never
//! watched: a splash screen or a paused black video is one colour on purpose.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

/// How long after the window manager starts the tasks that exist are taken as its
/// inheritance. A task Android reports later was started with this renderer.
pub const INHERIT_WITHIN: Duration = Duration::from_secs(15);

/// How long an inherited task's frames stay one colour before its window is closed. Long
/// enough for an app that does recover to draw its first new frame.
pub const BLANK_AFTER: Duration = Duration::from_secs(8);

/// How long after the window manager starts the inherited tasks are watched at all.
pub const WATCH_FOR: Duration = Duration::from_secs(120);

/// The tasks inherited from an earlier window manager and what their frames show.
#[derive(Debug)]
pub struct BlankWatch {
    started: Instant,
    inherited: BTreeSet<i32>,
    /// Since when each watched task's newest frame has been one colour.
    blank_since: BTreeMap<i32, Instant>,
}

impl BlankWatch {
    /// A watch for a window manager that starts at `started`.
    pub fn new(started: Instant) -> BlankWatch {
        BlankWatch {
            started,
            inherited: BTreeSet::new(),
            blank_since: BTreeMap::new(),
        }
    }

    /// Takes `tasks`, the tasks Android has at `now`, as inherited while the window manager
    /// is new. Later calls add nothing.
    pub fn inherit(&mut self, tasks: &[i32], now: Instant) {
        if now.saturating_duration_since(self.started) < INHERIT_WITHIN {
            self.inherited.extend(tasks.iter().copied());
        }
    }

    /// The tasks whose frames are looked at now: the inherited ones not yet seen recovered,
    /// while the watch lasts. Empty once it is over, so no frame is read for nothing.
    pub fn watched(&self, now: Instant) -> Vec<i32> {
        if now.saturating_duration_since(self.started) >= WATCH_FOR {
            return Vec::new();
        }
        self.inherited.iter().copied().collect()
    }

    /// The newest frame of `task_id` is one colour (`blank`) or not, as of `now`. A frame
    /// that is not one colour ends the watch of the task: it draws again.
    pub fn frame(&mut self, task_id: i32, blank: bool, now: Instant) {
        if !self.inherited.contains(&task_id) {
            return;
        }
        if blank {
            self.blank_since.entry(task_id).or_insert(now);
        } else {
            self.inherited.remove(&task_id);
            self.blank_since.remove(&task_id);
        }
    }

    /// The window of `task_id` is gone: it is not watched any more.
    pub fn forget(&mut self, task_id: i32) {
        self.inherited.remove(&task_id);
        self.blank_since.remove(&task_id);
    }

    /// The tasks whose newest frame has been one colour for [`BLANK_AFTER`] at `now`. Each is
    /// returned once and is not watched afterwards.
    pub fn due(&mut self, now: Instant) -> Vec<i32> {
        if now.saturating_duration_since(self.started) >= WATCH_FOR {
            self.inherited.clear();
            self.blank_since.clear();
            return Vec::new();
        }
        let due: Vec<i32> = self
            .blank_since
            .iter()
            .filter(|(_, since)| now.saturating_duration_since(**since) >= BLANK_AFTER)
            .map(|(task_id, _)| *task_id)
            .collect();
        for task_id in &due {
            self.forget(*task_id);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn after(start: Instant, secs: u64) -> Instant {
        start + Duration::from_secs(secs)
    }

    #[test]
    fn nfr5_an_inherited_window_that_stays_one_colour_is_closed_instead_of_staying_black() {
        let start = Instant::now();
        let mut watch = BlankWatch::new(start);
        watch.inherit(&[87, 86], after(start, 2));
        watch.frame(87, true, after(start, 3));
        assert!(watch.due(after(start, 10)).is_empty());
        assert_eq!(watch.due(after(start, 11)), vec![87]);
        // Once: the window is closed, so it is not named again.
        assert!(watch.due(after(start, 30)).is_empty());
    }

    #[test]
    fn nfr5_an_inherited_window_that_draws_again_is_left_open() {
        let start = Instant::now();
        let mut watch = BlankWatch::new(start);
        watch.inherit(&[87], after(start, 2));
        watch.frame(87, true, after(start, 3));
        watch.frame(87, false, after(start, 6));
        assert!(watch.due(after(start, 60)).is_empty());
        assert!(watch.watched(after(start, 7)).is_empty());
    }

    #[test]
    fn nfr5_a_window_of_a_task_started_after_the_window_manager_is_never_closed_for_its_colour() {
        let start = Instant::now();
        let mut watch = BlankWatch::new(start);
        watch.inherit(&[87], after(start, 2));
        // Task 90 appears after the inheritance ended: a splash screen is one colour.
        watch.inherit(&[87, 90], after(start, 40));
        watch.frame(90, true, after(start, 41));
        assert!(watch.due(after(start, 100)).is_empty());
        assert_eq!(watch.watched(after(start, 41)), vec![87]);
    }

    #[test]
    fn nfr5_the_watch_ends_with_its_time() {
        let start = Instant::now();
        let mut watch = BlankWatch::new(start);
        watch.inherit(&[87], after(start, 1));
        watch.frame(87, true, after(start, 2));
        assert!(watch.watched(after(start, WATCH_FOR.as_secs())).is_empty());
        assert!(watch.due(after(start, WATCH_FOR.as_secs())).is_empty());
    }

    #[test]
    fn nfr5_a_window_that_closed_is_not_watched() {
        let start = Instant::now();
        let mut watch = BlankWatch::new(start);
        watch.inherit(&[87], after(start, 1));
        watch.frame(87, true, after(start, 2));
        watch.forget(87);
        assert!(watch.due(after(start, 60)).is_empty());
    }
}
