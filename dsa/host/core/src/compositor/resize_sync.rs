//! Whether a window being dragged to a new size waits for a frame of that size before it
//! shows the size.
//!
//! The guest draws a frame of the new size 40 to 50 ms after it is told the size. If the
//! window showed the size at once, the last frame would be stretched over it for that long,
//! visibly, on every step of the drag. A window therefore holds back from showing a new size
//! until the frame of that size is in, up to [`BUDGET`]. A guest that never answers with a
//! frame of exactly the window's size (a size it rounds, say) would make every step of the
//! drag cost the whole budget, so after [`GIVE_UP_AFTER`] steps in a row that ran out of
//! time the window stops waiting until the next drag.

use std::time::Duration;

/// The longest a window waits for a frame of its new size on one step of a drag. Past a
/// frame interval or two the drag feels held back more than the stretch looks wrong.
pub const BUDGET: Duration = Duration::from_millis(60);

/// How many steps in a row may run out of time before the drag stops waiting.
pub const GIVE_UP_AFTER: u32 = 3;

/// What a window does next while it waits for a frame of its size.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// The frame shown is of the window's size: show the size.
    Show,
    /// Wait up to this long more for the frame.
    Wait(Duration),
    /// The budget is spent: show the size with the frame stretched.
    GiveUp,
}

/// What a window does `waited` after it took `view`, when the frame shown is `frame`.
pub fn step(view: (u32, u32), frame: (u32, u32), waited: Duration) -> Step {
    if view == frame {
        Step::Show
    } else if waited >= BUDGET {
        Step::GiveUp
    } else {
        Step::Wait(BUDGET - waited)
    }
}

/// One window's waiting, over a drag.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResizeSync {
    dragging: bool,
    timeouts: u32,
}

impl ResizeSync {
    pub fn new() -> ResizeSync {
        ResizeSync::default()
    }

    /// The user took hold of the window's edge.
    pub fn drag_started(&mut self) {
        self.dragging = true;
        self.timeouts = 0;
    }

    /// The user let go.
    pub fn drag_ended(&mut self) {
        self.dragging = false;
        self.timeouts = 0;
    }

    /// Whether a step of the drag waits for its frame. Only a drag waits: a resize the
    /// program makes has nobody holding the edge to hold back.
    pub fn should_wait(&self) -> bool {
        self.dragging && self.timeouts < GIVE_UP_AFTER
    }

    /// A step ended; `in_time` says whether its frame came within the budget.
    pub fn step_ended(&mut self, in_time: bool) {
        self.timeouts = if in_time { 0 } else { self.timeouts + 1 };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ms(n: u64) -> Duration {
        Duration::from_millis(n)
    }

    #[test]
    fn fr4_a_frame_of_the_windows_size_ends_the_wait_at_once() {
        assert_eq!(step((800, 600), (800, 600), ms(0)), Step::Show);
        assert_eq!(step((800, 600), (800, 600), ms(500)), Step::Show);
    }

    #[test]
    fn fr4_a_frame_of_another_size_is_waited_for_the_rest_of_the_budget() {
        assert_eq!(step((800, 600), (790, 600), ms(0)), Step::Wait(BUDGET));
        assert_eq!(step((800, 600), (790, 600), ms(45)), Step::Wait(ms(15)));
    }

    #[test]
    fn fr4_the_wait_gives_up_when_the_budget_is_spent() {
        assert_eq!(step((800, 600), (790, 600), BUDGET), Step::GiveUp);
        assert_eq!(step((800, 600), (0, 0), ms(1000)), Step::GiveUp);
    }

    #[test]
    fn fr4_only_a_drag_waits() {
        let mut sync = ResizeSync::new();
        assert!(!sync.should_wait());
        sync.drag_started();
        assert!(sync.should_wait());
        sync.drag_ended();
        assert!(!sync.should_wait());
    }

    #[test]
    fn fr4_a_drag_stops_waiting_after_steps_in_a_row_ran_out_of_time() {
        let mut sync = ResizeSync::new();
        sync.drag_started();
        for _ in 0..GIVE_UP_AFTER {
            assert!(sync.should_wait());
            sync.step_ended(false);
        }
        assert!(!sync.should_wait());
        // The next drag tries again.
        sync.drag_ended();
        sync.drag_started();
        assert!(sync.should_wait());
    }

    #[test]
    fn fr4_a_step_in_time_clears_the_steps_that_ran_out() {
        let mut sync = ResizeSync::new();
        sync.drag_started();
        sync.step_ended(false);
        sync.step_ended(false);
        sync.step_ended(true);
        sync.step_ended(false);
        sync.step_ended(false);
        assert!(sync.should_wait());
    }
}
