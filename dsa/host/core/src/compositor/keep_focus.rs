//! Keep game focus: which task holds the input focus inside Android while the user works in
//! another window, for the apps the user set to keep it.
//!
//! Android gives the input focus to one task at a time. Its app hears it has the focus
//! (`onWindowFocusChanged`) and its activity is the top resumed one
//! (`onTopResumedActivityChanged`); a game such as Monument Valley pauses when it loses
//! either. Without anything here the guest's focus moves only with the user's input: a press
//! in a task's window focuses that task, and a key focuses the task whose window sent it.
//! When the desktop focus goes to a window that is not a task's, Android is not told, so the
//! task that had the focus keeps it.
//!
//! For an app with keep focus on, [`FocusKeeper`] adds three rules, and only while such an
//! app has a task (so with every app off nothing changes at all):
//!
//! 1. When the app's window becomes the active one, its task is given the focus at once, also
//!    when the window was reached with Alt+Tab or the taskbar rather than a click.
//! 2. When another task's window becomes the active one while the kept app's task holds the
//!    focus, that task is given it: keys and text must reach the window the user is in, and
//!    Android sends them to the focused task. The kept app's task stays resumed, since the
//!    guest's tasks never cover each other, but it does hear that it lost the focus.
//! 3. When no task window is the active one (the user went to another program), the focus
//!    goes back to the kept app's task that was the last to be active, once the desktop has
//!    settled for [`SETTLE`]: a click from one task window to another deactivates the first
//!    before it activates the second, and the kept app must not take the focus in between.
//!
//! Keys and text never go to a window that is not the active one: the window host sends them
//! from the window that has the keyboard, and [`FocusKeeper::takes_keys`] says which that is.
//! The guest's focus is only where Android keeps the app's activity running.

use std::collections::{BTreeMap, BTreeSet};
use std::time::{Duration, Instant};

use crate::tasks::TaskRequest;

/// How long no task window has to be the active one before a kept app's task is given the
/// focus back: longer than Windows takes between deactivating one window and activating the
/// next, and short enough that the game is running again before the user looks back at it.
pub const SETTLE: Duration = Duration::from_millis(250);

/// Follows which task window is the active one, and says when the guest's input focus must
/// move for an app that keeps its focus. Platform neutral: a window manager reports what its
/// windows say and sends the requests it returns.
#[derive(Debug, Default)]
pub struct FocusKeeper {
    /// The packages whose tasks keep the focus, as the user's window settings say.
    kept: BTreeSet<String>,
    /// The package of each task whose window reported its focus, while the window is open.
    packages: BTreeMap<i32, String>,
    /// The task whose window is the active one on the desktop, if any is.
    active: Option<i32>,
    /// The task of a kept app whose window was the last active one, while it is open and its
    /// app still keeps the focus.
    keeper: Option<i32>,
    /// The task that holds the focus in Android as far as this knows: the last one whose
    /// window was active or that it focused.
    holder: Option<i32>,
    /// When the kept app's task is to be given the focus back, while no window is active.
    give_back_at: Option<Instant>,
}

impl FocusKeeper {
    pub fn new() -> FocusKeeper {
        FocusKeeper::default()
    }

    /// The packages that keep their focus now ([`crate::app_windows::keep_focus_packages`]).
    /// An app turned off stops being kept at once; its task keeps whatever focus it has until
    /// the user's input moves it, as for any other app.
    pub fn set_kept(&mut self, packages: &[String]) {
        self.kept = packages.iter().cloned().collect();
        if let Some(task) = self.keeper {
            if !self.is_kept(task) {
                self.keeper = None;
                self.give_back_at = None;
            }
        }
    }

    /// Whether `package` keeps its focus.
    pub fn keeps(&self, package: &str) -> bool {
        self.kept.contains(package)
    }

    fn is_kept(&self, task_id: i32) -> bool {
        self.packages
            .get(&task_id)
            .is_some_and(|package| self.kept.contains(package))
    }

    /// The window of `task_id`, a task of `package`, became the active one (`focused`) or
    /// stopped being it. Returns the focus request to send to the guest, if any.
    pub fn window_focus(
        &mut self,
        task_id: i32,
        package: Option<&str>,
        focused: bool,
        now: Instant,
    ) -> Vec<TaskRequest> {
        if let Some(package) = package {
            self.packages.insert(task_id, package.to_string());
        }
        if !focused {
            if self.active == Some(task_id) {
                self.active = None;
                if self.keeper.is_some() && self.keeper != self.holder {
                    self.give_back_at = Some(now + SETTLE);
                }
            }
            return Vec::new();
        }
        self.active = Some(task_id);
        self.give_back_at = None;
        let kept = self.is_kept(task_id);
        let holder_kept = self.holder.is_some_and(|holder| self.is_kept(holder));
        if kept {
            self.keeper = Some(task_id);
        }
        let previous = self.holder.replace(task_id);
        // Rules 1 and 2: the active window's task takes the focus when a kept app gains it or
        // loses it. Anything else is left to the user's own clicks and keys, as before.
        if previous != Some(task_id) && (kept || holder_kept) {
            vec![TaskRequest::Focus { task_id }]
        } else {
            Vec::new()
        }
    }

    /// Rule 3: once no window has been active for [`SETTLE`], the kept app's task takes the
    /// focus back. Call it on the window manager's tick.
    pub fn tick(&mut self, now: Instant) -> Vec<TaskRequest> {
        let Some(at) = self.give_back_at else {
            return Vec::new();
        };
        if now < at {
            return Vec::new();
        }
        self.give_back_at = None;
        match self.keeper {
            Some(task_id) if self.active.is_none() && self.holder != Some(task_id) => {
                self.holder = Some(task_id);
                vec![TaskRequest::Focus { task_id }]
            }
            _ => Vec::new(),
        }
    }

    /// When [`FocusKeeper::tick`] has something to do, for a window manager that sleeps
    /// until its next deadline.
    pub fn deadline(&self) -> Option<Instant> {
        self.give_back_at
    }

    /// The task's window closed: it is neither active nor kept any more.
    pub fn closed(&mut self, task_id: i32) {
        self.packages.remove(&task_id);
        if self.active == Some(task_id) {
            self.active = None;
        }
        if self.holder == Some(task_id) {
            self.holder = None;
        }
        if self.keeper == Some(task_id) {
            self.keeper = None;
            self.give_back_at = None;
        }
    }

    /// Forgets every task whose window is no longer open (`open` says false), as
    /// [`FocusKeeper::closed`] does for one.
    pub fn retain(&mut self, mut open: impl FnMut(i32) -> bool) {
        let gone: Vec<i32> = self
            .packages
            .keys()
            .copied()
            .chain(self.active)
            .chain(self.holder)
            .chain(self.keeper)
            .filter(|task_id| !open(*task_id))
            .collect();
        for task_id in gone {
            self.closed(task_id);
        }
    }

    /// Whether keys and text may go to the task's window: only while it is the active one,
    /// whether or not its app keeps its focus inside Android.
    pub fn takes_keys(&self, task_id: i32) -> bool {
        self.active == Some(task_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GAME: &str = "com.ustwo.monumentvalley";
    const NOTES: &str = "com.example.notes";

    fn focus(task_id: i32) -> Vec<TaskRequest> {
        vec![TaskRequest::Focus { task_id }]
    }

    fn keeping_the_game() -> FocusKeeper {
        let mut keeper = FocusKeeper::new();
        keeper.set_kept(&[GAME.to_string()]);
        keeper
    }

    #[test]
    fn fr44_an_app_that_is_off_never_moves_the_guests_focus() {
        let now = Instant::now();
        let mut keeper = FocusKeeper::new();
        assert!(!keeper.keeps(GAME));
        // The game is active, the user clicks another program, then a note's window, then
        // another program again: the focus follows the user's input alone, as before.
        assert!(keeper.window_focus(1, Some(GAME), true, now).is_empty());
        assert!(keeper.window_focus(1, Some(GAME), false, now).is_empty());
        assert!(keeper.tick(now + SETTLE * 4).is_empty());
        assert!(keeper.window_focus(2, Some(NOTES), true, now).is_empty());
        assert!(keeper.window_focus(2, Some(NOTES), false, now).is_empty());
        assert!(keeper.tick(now + SETTLE * 8).is_empty());
        assert_eq!(keeper.deadline(), None);
    }

    #[test]
    fn fr44_a_kept_apps_task_stays_focused_when_its_window_loses_the_desktop_focus() {
        let now = Instant::now();
        let mut keeper = keeping_the_game();
        // The game's window becomes active: its task takes the focus.
        assert_eq!(keeper.window_focus(1, Some(GAME), true, now), focus(1));
        assert!(keeper.takes_keys(1));
        // The user clicks another program: nothing takes the focus from the game's task, and
        // it is not given it again either, since it still holds it.
        assert!(keeper.window_focus(1, Some(GAME), false, now).is_empty());
        assert!(keeper.tick(now + SETTLE * 4).is_empty());
        // Its window does not have the keyboard, so keys do not go to it.
        assert!(!keeper.takes_keys(1));
    }

    #[test]
    fn fr44_another_task_window_takes_the_focus_and_the_game_gets_it_back_after() {
        let now = Instant::now();
        let mut keeper = keeping_the_game();
        keeper.window_focus(1, Some(GAME), true, now);
        // A click from the game into a note's window: the game's window deactivates first,
        // then the note's activates, and the note's task takes the focus at once, as keys
        // and text must reach it.
        assert!(keeper.window_focus(1, Some(GAME), false, now).is_empty());
        assert_eq!(keeper.window_focus(2, Some(NOTES), true, now), focus(2));
        assert!(keeper.takes_keys(2));
        assert!(!keeper.takes_keys(1));
        // Nothing gives the game the focus while the note is active.
        assert!(keeper.tick(now + SETTLE * 4).is_empty());
        // The user goes on to another program: once the desktop settles, the game takes the
        // focus back and runs again, and keys go to no task window.
        let left = now + SETTLE * 5;
        assert!(keeper.window_focus(2, Some(NOTES), false, left).is_empty());
        assert_eq!(keeper.deadline(), Some(left + SETTLE));
        assert!(keeper.tick(left + SETTLE / 2).is_empty());
        assert_eq!(keeper.tick(left + SETTLE), focus(1));
        assert!(!keeper.takes_keys(1));
        assert!(!keeper.takes_keys(2));
        // Once only.
        assert!(keeper.tick(left + SETTLE * 3).is_empty());
    }

    #[test]
    fn fr44_moving_between_task_windows_never_gives_the_game_the_focus_in_between() {
        let now = Instant::now();
        let mut keeper = keeping_the_game();
        keeper.window_focus(1, Some(GAME), true, now);
        keeper.window_focus(2, Some(NOTES), true, now);
        // From the note to a second note's window, deactivation first.
        assert!(keeper.window_focus(2, Some(NOTES), false, now).is_empty());
        assert!(keeper.window_focus(3, Some(NOTES), true, now).is_empty());
        assert!(keeper.tick(now + SETTLE * 2).is_empty());
        // And with the two reports the other way round.
        assert!(keeper.window_focus(2, Some(NOTES), true, now).is_empty());
        assert!(keeper.window_focus(3, Some(NOTES), false, now).is_empty());
        assert!(keeper.tick(now + SETTLE * 2).is_empty());
    }

    #[test]
    fn fr44_the_game_reached_without_a_click_takes_the_focus() {
        let now = Instant::now();
        let mut keeper = keeping_the_game();
        // The note is active and holds the focus; Alt+Tab to the game's window.
        assert!(keeper.window_focus(2, Some(NOTES), true, now).is_empty());
        assert!(keeper.window_focus(2, Some(NOTES), false, now).is_empty());
        assert_eq!(keeper.window_focus(1, Some(GAME), true, now), focus(1));
        // Back again: the note takes it, as the game held it.
        keeper.window_focus(1, Some(GAME), false, now);
        assert_eq!(keeper.window_focus(2, Some(NOTES), true, now), focus(2));
    }

    #[test]
    fn fr44_turning_keep_focus_off_or_closing_the_game_stops_giving_it_back() {
        let now = Instant::now();
        let mut keeper = keeping_the_game();
        keeper.window_focus(1, Some(GAME), true, now);
        keeper.window_focus(2, Some(NOTES), true, now);
        keeper.window_focus(2, Some(NOTES), false, now);
        keeper.set_kept(&[]);
        assert!(keeper.tick(now + SETTLE * 2).is_empty());

        let mut keeper = keeping_the_game();
        keeper.window_focus(1, Some(GAME), true, now);
        keeper.window_focus(2, Some(NOTES), true, now);
        keeper.window_focus(2, Some(NOTES), false, now);
        keeper.closed(1);
        assert!(keeper.tick(now + SETTLE * 2).is_empty());
        assert_eq!(keeper.deadline(), None);
    }
}
