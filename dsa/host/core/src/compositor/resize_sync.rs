//! Whether a window being dragged to a new size waits for a frame of that size before it
//! takes the size.
//!
//! The guest needs tens of milliseconds to draw at a new size, and how many depends on the
//! app. A window that took the size at once would show its last frame stretched for that
//! long, on every step of the drag. A window therefore asks the guest for the size first and
//! keeps its old size, with its old frame, while the frame of the new size is on its way, and
//! takes the size and shows the frame together. While it waits, the screen is whole: the old
//! window with the old frame.
//!
//! How long it waits follows how fast the task has been answering ([`ResizeSync::budget`]),
//! between [`MIN_BUDGET`] and [`MAX_BUDGET`]. Past that the drag feels held back more than a
//! stretch looks wrong, so the window then takes the size with the last frame stretched. A
//! guest that never answers with a frame of exactly the window's size would make every step
//! cost the whole budget, so after [`GIVE_UP_AFTER`] steps in a row that ran out of time the
//! window stops waiting. It starts again as soon as a frame shows the task can keep up, or
//! when a later step tries again ([`FIRST_RETRY`], then less and less often): a task is often
//! slow only at the start of a drag, and a drag that moves on never sees a frame of its
//! latest size unless it waits for one.

use std::time::{Duration, Instant};

/// How long a window waits on one step of a drag before it knows how fast its task answers.
pub const FIRST_BUDGET: Duration = Duration::from_millis(60);

/// The shortest wait on one step, so one quick frame does not make the next, ordinary one
/// late.
pub const MIN_BUDGET: Duration = Duration::from_millis(50);

/// The longest wait on one step: ten steps a second is as slow as a drag can be and still
/// follow the pointer.
pub const MAX_BUDGET: Duration = Duration::from_millis(100);

/// How many steps in a row may run out of time before the drag stops waiting.
pub const GIVE_UP_AFTER: u32 = 3;

/// How long after giving up a drag first waits again on a step. Each step that still runs out
/// of time doubles this, up to [`LAST_RETRY`].
pub const FIRST_RETRY: Duration = Duration::from_millis(250);

/// The longest a drag goes without trying to wait again.
pub const LAST_RETRY: Duration = Duration::from_secs(2);

/// The first frame measured counts as this much at most, and each later one as twice what is
/// typical, so one unusually slow frame (an app recreated, a garbage collection) does not push
/// the waits of the steps after it to the limit.
const LONGEST_SAMPLE: Duration = Duration::from_millis(300);

/// How many sizes asked for are remembered, to measure frames that arrive after the window
/// has moved on.
const ASKED: usize = 16;

/// What a window does next while it waits for a frame of its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The frame of the window's size is in: take the size.
    Show,
    /// Wait up to this long more for the frame.
    Wait(Duration),
    /// The budget is spent: take the size with the frame stretched.
    GiveUp,
}

/// What a window does `waited` after it asked for `view`, when the latest frame is `frame`.
pub fn step(view: (u32, u32), frame: (u32, u32), waited: Duration, budget: Duration) -> Step {
    if view == frame {
        Step::Show
    } else if waited >= budget {
        Step::GiveUp
    } else {
        Step::Wait(budget - waited)
    }
}

/// One window's waiting, over its drags.
#[derive(Debug, Clone)]
pub struct ResizeSync {
    dragging: bool,
    /// Steps in a row that ran out of time.
    timeouts: u32,
    /// When a drag that gave up waits on a step again.
    retry_at: Option<Instant>,
    /// How long after the next give-up a drag waits again.
    retry_after: Duration,
    /// How long the task typically takes to answer a size, once a frame has shown it. Kept
    /// from one drag to the next: the app is the same.
    typical: Option<Duration>,
    /// The sizes asked for that have no frame yet, oldest first.
    asked: Vec<((u32, u32), Instant)>,
}

impl Default for ResizeSync {
    fn default() -> ResizeSync {
        ResizeSync {
            dragging: false,
            timeouts: 0,
            retry_at: None,
            retry_after: FIRST_RETRY,
            typical: None,
            asked: Vec::with_capacity(ASKED),
        }
    }
}

impl ResizeSync {
    pub fn new() -> ResizeSync {
        ResizeSync::default()
    }

    fn start_over(&mut self) {
        self.timeouts = 0;
        self.retry_at = None;
        self.retry_after = FIRST_RETRY;
    }

    /// The user took hold of the window's edge.
    pub fn drag_started(&mut self) {
        self.dragging = true;
        self.start_over();
        self.asked.clear();
    }

    /// The user let go.
    pub fn drag_ended(&mut self) {
        self.dragging = false;
        self.start_over();
    }

    /// Whether a step of the drag at `now` waits for its frame. Only a drag waits: a resize
    /// the program makes has nobody holding the edge to hold back.
    pub fn should_wait(&self, now: Instant) -> bool {
        self.dragging
            && (self.timeouts < GIVE_UP_AFTER || self.retry_at.is_some_and(|at| now >= at))
    }

    /// How long a step waits for its frame: half as long again as the task typically takes,
    /// so an ordinary frame comes in time.
    pub fn budget(&self) -> Duration {
        match self.typical {
            Some(typical) => (typical * 3 / 2).clamp(MIN_BUDGET, MAX_BUDGET),
            None => FIRST_BUDGET,
        }
    }

    /// A step ended at `now`; `in_time` says whether its frame came within the budget.
    pub fn step_ended(&mut self, in_time: bool, now: Instant) {
        if in_time {
            self.start_over();
            return;
        }
        self.timeouts = self.timeouts.saturating_add(1);
        if self.timeouts >= GIVE_UP_AFTER {
            self.retry_at = Some(now + self.retry_after);
            self.retry_after = (self.retry_after * 2).min(LAST_RETRY);
        }
    }

    /// The guest was asked for `size` at `now`.
    pub fn size_asked(&mut self, size: (u32, u32), now: Instant) {
        if self.asked.last().is_some_and(|(last, _)| *last == size) {
            return;
        }
        if self.asked.len() == ASKED {
            self.asked.remove(0);
        }
        self.asked.push((size, now));
    }

    /// A frame of `size` arrived at `now`. Returns how long after the size was asked for,
    /// when it is the answer to a size still open. A frame that answers within the budget
    /// shows the task keeps up, so a drag that had given up waits again.
    pub fn frame_arrived(&mut self, size: (u32, u32), now: Instant) -> Option<Duration> {
        let found = self.asked.iter().rposition(|(asked, _)| *asked == size)?;
        let waited = now.saturating_duration_since(self.asked[found].1);
        // The guest draws the latest size it was told, so the older ones will not come.
        self.asked.drain(..=found);
        let sample = waited.min(LONGEST_SAMPLE);
        self.typical = Some(match self.typical {
            Some(typical) => (typical * 3 + sample.min(typical * 2)) / 4,
            None => sample,
        });
        if waited <= self.budget() {
            self.start_over();
        }
        Some(waited)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    /// A drag whose steps ran out of time until it gave up.
    fn given_up(now: Instant) -> ResizeSync {
        let mut sync = ResizeSync::new();
        sync.drag_started();
        for _ in 0..GIVE_UP_AFTER {
            sync.step_ended(false, now);
        }
        sync
    }

    #[test]
    fn fr4_a_frame_of_the_windows_size_ends_the_wait_at_once() {
        let budget = FIRST_BUDGET;
        assert_eq!(step((800, 600), (800, 600), ms(0), budget), Step::Show);
        assert_eq!(step((800, 600), (800, 600), ms(500), budget), Step::Show);
    }

    #[test]
    fn fr4_a_frame_of_another_size_is_waited_for_the_rest_of_the_budget() {
        assert_eq!(
            step((800, 600), (790, 600), ms(0), ms(60)),
            Step::Wait(ms(60))
        );
        assert_eq!(
            step((800, 600), (790, 600), ms(45), ms(60)),
            Step::Wait(ms(15))
        );
    }

    #[test]
    fn fr4_the_wait_gives_up_when_the_budget_is_spent() {
        assert_eq!(step((800, 600), (790, 600), ms(60), ms(60)), Step::GiveUp);
        assert_eq!(step((800, 600), (0, 0), ms(1000), ms(60)), Step::GiveUp);
    }

    #[test]
    fn fr4_only_a_drag_waits() {
        let now = Instant::now();
        let mut sync = ResizeSync::new();
        assert!(!sync.should_wait(now));
        sync.drag_started();
        assert!(sync.should_wait(now));
        sync.drag_ended();
        assert!(!sync.should_wait(now));
    }

    #[test]
    fn fr4_a_drag_stops_waiting_after_steps_in_a_row_ran_out_of_time() {
        let now = Instant::now();
        let mut sync = given_up(now);
        assert!(!sync.should_wait(now));
        // The next drag tries again.
        sync.drag_ended();
        sync.drag_started();
        assert!(sync.should_wait(now));
    }

    #[test]
    fn fr4_a_step_in_time_clears_the_steps_that_ran_out() {
        let now = Instant::now();
        let mut sync = ResizeSync::new();
        sync.drag_started();
        sync.step_ended(false, now);
        sync.step_ended(false, now);
        sync.step_ended(true, now);
        sync.step_ended(false, now);
        sync.step_ended(false, now);
        assert!(sync.should_wait(now));
    }

    #[test]
    fn fr4_a_drag_that_gave_up_tries_again_later_in_the_same_drag() {
        let start = Instant::now();
        let mut sync = given_up(start);
        assert!(!sync.should_wait(start + FIRST_RETRY / 2));
        assert!(sync.should_wait(start + FIRST_RETRY));
        // The try ran out of time too: the next comes after twice as long.
        let tried = start + FIRST_RETRY;
        sync.step_ended(false, tried);
        assert!(!sync.should_wait(tried + FIRST_RETRY));
        assert!(sync.should_wait(tried + FIRST_RETRY * 2));
        // It came in time: the drag waits on every step again.
        let again = tried + FIRST_RETRY * 2;
        sync.step_ended(true, again);
        assert!(sync.should_wait(again));
    }

    #[test]
    fn fr4_a_guest_that_never_answers_is_tried_at_most_every_two_seconds() {
        let mut now = Instant::now();
        let mut sync = given_up(now);
        for _ in 0..8 {
            now += LAST_RETRY;
            assert!(sync.should_wait(now));
            sync.step_ended(false, now);
            assert!(!sync.should_wait(now));
        }
    }

    #[test]
    fn fr4_a_frame_in_time_lets_a_drag_that_gave_up_wait_again() {
        let start = Instant::now();
        let mut sync = given_up(start);
        sync.size_asked((500, 300), start);
        assert_eq!(sync.frame_arrived((500, 300), start + ms(40)), Some(ms(40)));
        assert!(sync.should_wait(start + ms(40)));
    }

    #[test]
    fn fr4_a_late_frame_does_not_let_a_drag_that_gave_up_wait_again() {
        let start = Instant::now();
        let mut sync = given_up(start);
        sync.size_asked((500, 300), start);
        sync.frame_arrived((500, 300), start + ms(120));
        // 120 ms from the first frame known: more than the longest wait.
        assert!(!sync.should_wait(start + ms(120)));
    }

    #[test]
    fn fr4_the_wait_follows_how_fast_the_task_answers() {
        let start = Instant::now();
        let mut sync = ResizeSync::new();
        assert_eq!(sync.budget(), FIRST_BUDGET);
        // Answers in 55 ms, as a task on a real machine does: a little more is waited.
        sync.size_asked((500, 300), start);
        sync.frame_arrived((500, 300), start + ms(55));
        assert_eq!(sync.budget(), ms(55) * 3 / 2);
        // A quick task still gets the shortest wait.
        let mut quick = ResizeSync::new();
        quick.size_asked((500, 300), start);
        quick.frame_arrived((500, 300), start + ms(10));
        assert_eq!(quick.budget(), MIN_BUDGET);
        // A slow one never holds the drag longer than the longest.
        let mut slow = ResizeSync::new();
        slow.size_asked((500, 300), start);
        slow.frame_arrived((500, 300), start + ms(2000));
        assert_eq!(slow.budget(), MAX_BUDGET);
    }

    #[test]
    fn fr4_one_slow_frame_does_not_take_the_wait_to_its_limit() {
        let start = Instant::now();
        let mut sync = ResizeSync::new();
        for step in 0..8u32 {
            let at = start + ms(u64::from(step) * 100);
            sync.size_asked((500 + step, 300), at);
            sync.frame_arrived((500 + step, 300), at + ms(50));
        }
        let at = start + ms(1000);
        sync.size_asked((600, 300), at);
        sync.frame_arrived((600, 300), at + ms(2770));
        assert!(sync.budget() < MAX_BUDGET, "{:?}", sync.budget());
    }

    #[test]
    fn fr4_a_frame_that_arrives_after_the_window_moved_on_is_still_measured() {
        let start = Instant::now();
        let mut sync = ResizeSync::new();
        sync.size_asked((500, 300), start);
        sync.size_asked((510, 300), start + ms(20));
        sync.size_asked((520, 300), start + ms(40));
        assert_eq!(sync.frame_arrived((510, 300), start + ms(90)), Some(ms(70)));
        // The older size is past; the frame after it answers only the newest.
        assert_eq!(sync.frame_arrived((500, 300), start + ms(95)), None);
        assert_eq!(
            sync.frame_arrived((520, 300), start + ms(100)),
            Some(ms(60))
        );
        // Another frame of a size already answered measures nothing.
        assert_eq!(sync.frame_arrived((520, 300), start + ms(120)), None);
    }

    #[test]
    fn fr4_the_same_size_asked_twice_is_measured_from_the_first() {
        let start = Instant::now();
        let mut sync = ResizeSync::new();
        sync.size_asked((500, 300), start);
        sync.size_asked((500, 300), start + ms(30));
        assert_eq!(sync.frame_arrived((500, 300), start + ms(50)), Some(ms(50)));
    }
}
