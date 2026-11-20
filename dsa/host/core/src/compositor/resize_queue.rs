//! The requests on their way to the guest, as the sizes of a live resize need them: folded
//! together, and paced to the rate the app draws.
//!
//! A window asks for a new size every few milliseconds while the user drags its edge, and each
//! request is a round trip to the guest. Sent one after the other they queue up: the guest
//! worked through every size the user had dragged past, and the size the drag ended on was
//! drawn seconds after the mouse went up. So only the latest size of a task is kept, and it
//! goes once a frame of the size before has been drawn (or after a short patience).

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::resize_pacing::ResizePacer;
use crate::tasks::{coalesce, TaskRequest};

/// A task id and the size, in pixels, one of its frames was drawn at.
type DrawnSize = (i32, (u32, u32));

/// How often the request thread looks for drawn frames while a size of a drag is held back.
pub const PACE_TICK: Duration = Duration::from_millis(4);

/// The frames the apps drew since the request thread last looked: task and size.
///
/// Clones share one list: the thread that receives the frames keeps a clone and reports each
/// one, and the [`ResizeQueue`] made from another clone sees them. Two lists never mix.
#[derive(Clone, Default)]
pub struct DrawnFrames {
    drawn: Arc<Mutex<Vec<DrawnSize>>>,
}

impl DrawnFrames {
    pub fn new() -> DrawnFrames {
        DrawnFrames::default()
    }

    /// The app of `task_id` drew a frame of `size`: a size held back for it may go. Only the
    /// latest frame of a task is kept.
    pub fn frame_drawn(&self, task_id: i32, size: (u32, u32)) {
        let mut drawn = self.drawn.lock().unwrap_or_else(|e| e.into_inner());
        match drawn.iter_mut().find(|(id, _)| *id == task_id) {
            Some(entry) => entry.1 = size,
            None => drawn.push((task_id, size)),
        }
    }

    fn take(&self) -> Vec<DrawnSize> {
        std::mem::take(&mut *self.drawn.lock().unwrap_or_else(|e| e.into_inner()))
    }
}

/// Folds and paces requests.
pub struct ResizeQueue {
    drawn: DrawnFrames,
    pacer: ResizePacer,
    held: Vec<TaskRequest>,
}

impl ResizeQueue {
    /// A queue that learns which frames were drawn from `drawn`.
    pub fn new(drawn: DrawnFrames) -> ResizeQueue {
        ResizeQueue {
            drawn,
            pacer: ResizePacer::new(),
            held: Vec::new(),
        }
    }

    /// Whether sizes are held back for a frame.
    pub fn waiting(&self) -> bool {
        !self.held.is_empty()
    }

    /// The requests held back for a frame.
    pub fn held(&self) -> &[TaskRequest] {
        &self.held
    }

    /// `incoming` joins what is held: returns what goes to the guest now, in order. Held sizes
    /// come first, so a newer size of the same task replaces them. A size that ends a drag
    /// (not live) always goes.
    pub fn step(&mut self, incoming: Vec<TaskRequest>, now: Instant) -> Vec<TaskRequest> {
        for (task_id, size) in self.drawn.take() {
            self.pacer.frame_drawn(task_id, size);
        }
        let mut waiting = std::mem::take(&mut self.held);
        waiting.extend(incoming);
        let (go, held) = self.pacer.pace(coalesce(waiting), now);
        self.held = held;
        go
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn live_of(task_id: i32, width: u32) -> TaskRequest {
        TaskRequest::Resize {
            task_id,
            width,
            height: 600,
            live: true,
        }
    }

    fn live(width: u32) -> TaskRequest {
        live_of(7, width)
    }

    #[test]
    fn fr4_a_drag_of_a_hundred_sizes_sends_one_and_then_the_latest() {
        let start = Instant::now();
        let drawn = DrawnFrames::new();
        let mut queue = ResizeQueue::new(drawn.clone());
        let first = queue.step(vec![live(601)], start);
        assert_eq!(first, vec![live(601)]);
        // The user drags on while the app draws the first size: nothing more goes.
        for (n, width) in (602..700).enumerate() {
            let later = start + Duration::from_millis(n as u64);
            assert!(queue.step(vec![live(width)], later).is_empty());
        }
        assert!(queue.waiting());
        assert_eq!(queue.held(), &[live(699)]);
        // The app drew the first size: only the latest of the rest goes.
        drawn.frame_drawn(7, (601, 600));
        let next = queue.step(Vec::new(), start + Duration::from_millis(120));
        assert_eq!(next, vec![live(699)]);
        assert!(!queue.waiting());
    }

    #[test]
    fn fr4_the_size_that_ends_a_drag_goes_at_once() {
        let start = Instant::now();
        let mut queue = ResizeQueue::new(DrawnFrames::new());
        queue.step(vec![live(601)], start);
        let end = TaskRequest::Resize {
            task_id: 7,
            width: 900,
            height: 600,
            live: false,
        };
        assert_eq!(
            queue.step(
                vec![live(650), end.clone()],
                start + Duration::from_millis(5)
            ),
            vec![end]
        );
    }

    #[test]
    fn fr4_a_held_size_goes_after_the_patience_without_a_drawn_frame() {
        let start = Instant::now();
        let mut queue = ResizeQueue::new(DrawnFrames::new());
        queue.step(vec![live(601)], start);
        assert!(queue
            .step(vec![live(650)], start + Duration::from_millis(5))
            .is_empty());
        let later = start + super::super::resize_pacing::PATIENCE;
        assert_eq!(queue.step(Vec::new(), later), vec![live(650)]);
    }

    #[test]
    fn fr4_drawn_frames_of_two_queues_do_not_mix() {
        let start = Instant::now();
        let first_drawn = DrawnFrames::new();
        let mut first = ResizeQueue::new(first_drawn.clone());
        let mut second = ResizeQueue::new(DrawnFrames::new());
        for queue in [&mut first, &mut second] {
            queue.step(vec![live(601)], start);
            queue.step(vec![live(650)], start + Duration::from_millis(5));
        }
        // Only the first queue's frames were reported: the second still waits.
        first_drawn.frame_drawn(7, (601, 600));
        let soon = start + Duration::from_millis(10);
        assert_eq!(first.step(Vec::new(), soon), vec![live(650)]);
        assert!(second.step(Vec::new(), soon).is_empty());
        assert!(second.waiting());
    }

    #[test]
    fn fr4_tasks_are_paced_on_their_own() {
        let start = Instant::now();
        let drawn = DrawnFrames::new();
        let mut queue = ResizeQueue::new(drawn.clone());
        queue.step(vec![live_of(1, 601)], start);
        let soon = start + Duration::from_millis(5);
        // Another task's drag is not held back by the first one's wait.
        assert_eq!(
            queue.step(vec![live_of(2, 700), live_of(1, 650)], soon),
            vec![live_of(2, 700)]
        );
        // A frame of another size than the one waited for does not release it.
        drawn.frame_drawn(1, (999, 600));
        assert!(queue.step(Vec::new(), soon).is_empty());
    }
}
