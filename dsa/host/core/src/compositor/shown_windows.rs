//! Whether any task window is shown, which is what lets the guest sleep.
//!
//! A task window counts as shown while it is open and not minimized; a window covered by
//! other windows still counts, since the user can see it again at any moment without
//! telling the window manager. [`ShownWindows`] follows the windows the window manager has
//! and the windows their hosts report minimized, and says when the answer to "is any window
//! shown" changes, as the [`TaskRequest::WindowsShown`] that carries it to the guest.

use std::collections::BTreeSet;

use crate::tasks::TaskRequest;

/// The minimized task windows, and the last word the guest was given.
#[derive(Debug, Default)]
pub struct ShownWindows {
    minimized: BTreeSet<i32>,
    told: Option<bool>,
}

impl ShownWindows {
    pub fn new() -> ShownWindows {
        ShownWindows::default()
    }

    /// The window host reports a task's window minimized (`shown` false) or shown again.
    pub fn reported(&mut self, task_id: i32, shown: bool) {
        if shown {
            self.minimized.remove(&task_id);
        } else {
            self.minimized.insert(task_id);
        }
    }

    /// Forgets what the guest was told, so the next [`update`](Self::update) says it again,
    /// as when the service may have restarted and lost it.
    pub fn retell(&mut self) {
        self.told = None;
    }

    /// Given the tasks that have a window now, the request to send when whether any window
    /// is shown changed since the guest was last told, or it was never told.
    pub fn update(&mut self, windows: &[i32]) -> Option<TaskRequest> {
        // A window that closed is no longer minimized; one that opens again opens shown.
        self.minimized.retain(|task_id| windows.contains(task_id));
        let shown = windows
            .iter()
            .any(|task_id| !self.minimized.contains(task_id));
        if self.told == Some(shown) {
            return None;
        }
        self.told = Some(shown);
        Some(TaskRequest::WindowsShown { shown })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn shown(shown: bool) -> Option<TaskRequest> {
        Some(TaskRequest::WindowsShown { shown })
    }

    #[test]
    fn fr10_the_guest_is_told_at_once_whether_any_window_is_shown() {
        let mut windows = ShownWindows::new();
        assert_eq!(windows.update(&[]), shown(false));
        assert_eq!(windows.update(&[]), None);
        assert_eq!(windows.update(&[3]), shown(true));
        assert_eq!(windows.update(&[3]), None);
    }

    #[test]
    fn fr10_closing_or_minimizing_every_window_says_none_is_shown() {
        let mut windows = ShownWindows::new();
        assert_eq!(windows.update(&[1, 2]), shown(true));
        windows.reported(1, false);
        assert_eq!(windows.update(&[1, 2]), None, "window 2 is still shown");
        // Window 2 closes: only a minimized window is left.
        assert_eq!(windows.update(&[1]), shown(false));
        // Restoring it shows it again.
        windows.reported(1, true);
        assert_eq!(windows.update(&[1]), shown(true));
    }

    #[test]
    fn fr10_a_window_that_closes_while_minimized_opens_again_shown() {
        let mut windows = ShownWindows::new();
        assert_eq!(windows.update(&[1]), shown(true));
        windows.reported(1, false);
        assert_eq!(windows.update(&[1]), shown(false));
        assert_eq!(windows.update(&[]), None);
        assert_eq!(windows.update(&[1]), shown(true));
    }

    #[test]
    fn fr10_the_word_is_said_again_after_the_service_comes_back() {
        let mut windows = ShownWindows::new();
        assert_eq!(windows.update(&[1]), shown(true));
        windows.retell();
        assert_eq!(windows.update(&[1]), shown(true));
    }
}
