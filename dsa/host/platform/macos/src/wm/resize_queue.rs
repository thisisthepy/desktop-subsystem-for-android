//! The requests on their way to the guest, as the sizes of a live resize need them: folded
//! together, and paced to the rate the app draws.
//!
//! A window asks for a new size every few milliseconds while the user drags its edge, and each
//! request is a round trip to the guest. Sent one after the other they queue up: the guest
//! worked through every size the user had dragged past, and the size the drag ended on was
//! drawn seconds after the mouse went up. So only the latest size of a task is kept, and it
//! goes once a frame of the size before has been drawn (or after a short patience).

use std::sync::Mutex;
use std::time::Instant;

use dsa_host_core::compositor::resize_pacing::ResizePacer;
use dsa_host_core::tasks::{coalesce, TaskRequest};

/// The frames the apps drew since the request thread last looked: task and size.
static DRAWN: Mutex<Vec<(i32, (u32, u32))>> = Mutex::new(Vec::new());

/// The app of `task_id` drew a frame of `size`: a size held back for it may go.
pub fn frame_drawn(task_id: i32, size: (u32, u32)) {
    let mut drawn = DRAWN.lock().unwrap_or_else(|e| e.into_inner());
    match drawn.iter_mut().find(|(id, _)| *id == task_id) {
        Some(entry) => entry.1 = size,
        None => drawn.push((task_id, size)),
    }
}

/// Folds and paces requests.
#[derive(Default)]
pub struct ResizeQueue {
    pacer: ResizePacer,
    held: Vec<TaskRequest>,
}

impl ResizeQueue {
    pub fn new() -> ResizeQueue {
        ResizeQueue::default()
    }

    /// Whether sizes are held back for a frame.
    pub fn waiting(&self) -> bool {
        !self.held.is_empty()
    }

    /// `incoming` joins what is held: returns what goes to the guest now, in order. A size
    /// that ends a drag (not live) always goes.
    pub fn step(&mut self, incoming: Vec<TaskRequest>, now: Instant) -> Vec<TaskRequest> {
        let drawn = std::mem::take(&mut *DRAWN.lock().unwrap_or_else(|e| e.into_inner()));
        for (task_id, size) in drawn {
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
    use std::time::Duration;

    fn live(width: u32) -> TaskRequest {
        TaskRequest::Resize {
            task_id: 7,
            width,
            height: 600,
            live: true,
        }
    }

    #[test]
    fn fr4_a_drag_of_a_hundred_sizes_sends_one_and_then_the_latest() {
        let start = Instant::now();
        let mut queue = ResizeQueue::new();
        let first = queue.step(vec![live(601)], start);
        assert_eq!(first, vec![live(601)]);
        // The user drags on while the app draws the first size: nothing more goes.
        for (n, width) in (602..700).enumerate() {
            let later = start + Duration::from_millis(n as u64);
            assert!(queue.step(vec![live(width)], later).is_empty());
        }
        assert!(queue.waiting());
        // The app drew the first size: only the latest of the rest goes.
        frame_drawn(7, (601, 600));
        let next = queue.step(Vec::new(), start + Duration::from_millis(120));
        assert_eq!(next, vec![live(699)]);
    }

    #[test]
    fn fr4_the_size_that_ends_a_drag_goes_at_once() {
        let start = Instant::now();
        let mut queue = ResizeQueue::new();
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
}
