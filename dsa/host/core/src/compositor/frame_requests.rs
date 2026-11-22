//! When a window that has no frame yet asks the guest for its task's frame.
//!
//! SurfaceFlinger presents only when something on the guest's display changes. A window the
//! host opens for a task that already has its picture (the agent restarted, and the host
//! reopened the windows of the tasks that outlived it) would otherwise wait for its app to
//! draw again, and a still app (a dialog, a settings page) never does: the window would keep
//! its splash. So the window asks for the task's frame (`RequestTaskFrame`) once, as soon as
//! it is open and its task's display is known, and the hwcomposer sends the task's current
//! frame even when nothing changed. The first frame that reaches the window ends it. If none
//! does in [`FRAME_RETRY`] (the request was lost, or the window was not ready to show it), it
//! asks again, at most [`FRAME_ASKS`] times in all, so a task that cannot be drawn costs a
//! few presents and no more.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

/// How long a window waits for the frame it asked for before it asks again.
pub const FRAME_RETRY: Duration = Duration::from_millis(1000);

/// How many times in all a window asks for its task's frame.
pub const FRAME_ASKS: u32 = 3;

#[derive(Debug, Clone, Copy, Default)]
struct Waiting {
    /// When the frame was last asked for; `None` until it is.
    asked: Option<Instant>,
    asks: u32,
}

/// The windows that have no frame yet.
#[derive(Debug, Default)]
pub struct FrameRequests {
    waiting: BTreeMap<i32, Waiting>,
}

impl FrameRequests {
    pub fn new() -> FrameRequests {
        FrameRequests::default()
    }

    /// A window opened (or opened again) for `task_id`: it has no frame.
    pub fn opened(&mut self, task_id: i32) {
        self.waiting.insert(task_id, Waiting::default());
    }

    /// A frame of `task_id` reached its window.
    pub fn arrived(&mut self, task_id: i32) {
        self.waiting.remove(&task_id);
    }

    /// The task's window closed.
    pub fn closed(&mut self, task_id: i32) {
        self.waiting.remove(&task_id);
    }

    /// Keeps only the windows of the tasks `keep` says are still there.
    pub fn retain(&mut self, keep: impl Fn(i32) -> bool) {
        self.waiting.retain(|task_id, _| keep(*task_id));
    }

    /// Forgets every window.
    pub fn clear(&mut self) {
        self.waiting.clear();
    }

    /// Whether `task_id`'s window still waits for a frame.
    pub fn waiting(&self, task_id: i32) -> bool {
        self.waiting.contains_key(&task_id)
    }

    /// The tasks whose frame is to be asked for now. `ready` says whether a task's frame can
    /// be asked for yet: its window is there and its display is known. A window that asked
    /// [`FRAME_ASKS`] times and waited [`FRAME_RETRY`] after the last is given up on, and its
    /// task is in the second list, for the log.
    pub fn due(&mut self, now: Instant, ready: impl Fn(i32) -> bool) -> (Vec<i32>, Vec<i32>) {
        let mut ask = Vec::new();
        let mut given_up = Vec::new();
        for (task_id, waiting) in &mut self.waiting {
            if !ready(*task_id) {
                continue;
            }
            let retry_due = waiting
                .asked
                .is_none_or(|asked| now.saturating_duration_since(asked) >= FRAME_RETRY);
            if !retry_due {
                continue;
            }
            if waiting.asks >= FRAME_ASKS {
                given_up.push(*task_id);
                continue;
            }
            waiting.asked = Some(now);
            waiting.asks += 1;
            ask.push(*task_id);
        }
        for task_id in &given_up {
            self.waiting.remove(task_id);
        }
        (ask, given_up)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr3_a_reopened_window_without_a_frame_asks_exactly_once() {
        let start = Instant::now();
        let mut requests = FrameRequests::new();
        requests.opened(5);
        assert_eq!(requests.due(start, |_| true), (vec![5], vec![]));
        // Before the retry is due, however often it is asked, nothing more goes.
        for ms in [0, 1, 16, 500, 999] {
            let later = start + Duration::from_millis(ms);
            assert_eq!(requests.due(later, |_| true), (vec![], vec![]));
        }
    }

    #[test]
    fn fr3_a_frame_arriving_stops_the_requests() {
        let start = Instant::now();
        let mut requests = FrameRequests::new();
        requests.opened(5);
        requests.due(start, |_| true);
        requests.arrived(5);
        assert!(!requests.waiting(5));
        let later = start + FRAME_RETRY * 4;
        assert_eq!(requests.due(later, |_| true), (vec![], vec![]));
    }

    #[test]
    fn fr3_a_window_without_a_frame_asks_again_a_bounded_number_of_times() {
        let start = Instant::now();
        let mut requests = FrameRequests::new();
        requests.opened(5);
        let mut asks = 0;
        let mut given_up = Vec::new();
        for step in 0..20u32 {
            let (ask, gone) = requests.due(start + FRAME_RETRY * step, |_| true);
            asks += ask.len();
            given_up.extend(gone);
        }
        assert_eq!(asks, FRAME_ASKS as usize);
        assert_eq!(given_up, vec![5]);
        assert!(!requests.waiting(5));
    }

    #[test]
    fn fr3_a_window_whose_display_is_not_known_waits_to_ask() {
        let start = Instant::now();
        let mut requests = FrameRequests::new();
        requests.opened(5);
        assert_eq!(requests.due(start, |_| false), (vec![], vec![]));
        // Once it is known, the first ask goes at once.
        let later = start + Duration::from_millis(10);
        assert_eq!(requests.due(later, |_| true), (vec![5], vec![]));
    }

    #[test]
    fn fr3_a_window_opened_again_asks_again() {
        let start = Instant::now();
        let mut requests = FrameRequests::new();
        requests.opened(5);
        requests.due(start, |_| true);
        requests.arrived(5);
        requests.opened(5);
        assert_eq!(requests.due(start, |_| true), (vec![5], vec![]));
    }
}
