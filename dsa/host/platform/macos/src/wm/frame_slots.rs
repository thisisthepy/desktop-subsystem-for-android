//! The pacing of one task's frames between the renderer and the app's window host.
//!
//! Each task has two shared frame surfaces. The window host reads the one it was last told
//! of until it answers that the frame is on screen; the renderer meanwhile writes the other.
//! A frame that is ready while the window host still reads waits, and a newer frame written
//! over it replaces it, so the window always catches up to the newest frame and the guest
//! never waits for a window. A window host that does not answer in time is not waited for any
//! longer.

use std::time::Instant;

/// How many surfaces each task has.
pub const SLOTS: usize = 2;

/// How long a window host may take to show a frame before the next one goes without its
/// answer: the same on every host.
pub use dsa_host_core::compositor::window_sizing::PRESENT_WAIT;

/// A frame to announce to the window host.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Announce {
    /// The surface the frame is in.
    pub slot: usize,
    pub sequence: u64,
    /// The region from the surface's top left that the frame fills.
    pub content: (u32, u32),
}

/// One task's frames on their way to its window host.
#[derive(Debug)]
pub struct FrameSlots {
    /// The surface the window host reads, the frame's sequence, and since when.
    in_flight: Option<(usize, u64, Instant)>,
    /// The surface the renderer writes now.
    writing: Option<usize>,
    /// A frame that is written and waits for the window host.
    ready: Option<(usize, (u32, u32))>,
    next_sequence: u64,
}

impl Default for FrameSlots {
    fn default() -> FrameSlots {
        FrameSlots::new()
    }
}

impl FrameSlots {
    pub fn new() -> FrameSlots {
        FrameSlots {
            in_flight: None,
            writing: None,
            ready: None,
            next_sequence: 1,
        }
    }

    /// The renderer has a new frame: the surface to write it into, never the one the window
    /// host reads. A frame that waited in it is replaced by this one.
    pub fn begin(&mut self) -> usize {
        let slot = match self.in_flight {
            Some((0, _, _)) => 1,
            _ => 0,
        };
        if self.ready.is_some_and(|(ready, _)| ready == slot) {
            self.ready = None;
        }
        self.writing = Some(slot);
        slot
    }

    /// The renderer could not write the frame it began.
    pub fn abandon(&mut self) {
        self.writing = None;
    }

    /// The frame is in `slot`, filling `content` from its top left: what to announce now,
    /// if the window host is not reading another frame.
    pub fn written(&mut self, slot: usize, content: (u32, u32), now: Instant) -> Option<Announce> {
        if self.writing == Some(slot) {
            self.writing = None;
        }
        self.ready = Some((slot, content));
        self.send_ready(now)
    }

    /// The window host showed frame `sequence`: the frame that waited, if any, goes next. An
    /// answer for any other frame (one from before the window host reconnected) is ignored.
    pub fn presented(&mut self, sequence: u64, now: Instant) -> Option<Announce> {
        match self.in_flight {
            Some((_, in_flight, _)) if in_flight == sequence => {}
            _ => return None,
        }
        self.in_flight = None;
        self.send_ready(now)
    }

    /// The window host has not answered within [`PRESENT_WAIT`]: the waiting frame goes
    /// without its answer.
    pub fn overdue(&mut self, now: Instant) -> Option<Announce> {
        match self.in_flight {
            Some((_, _, since)) if now.saturating_duration_since(since) >= PRESENT_WAIT => {}
            _ => return None,
        }
        self.ready?;
        self.in_flight = None;
        self.send_ready(now)
    }

    /// The surfaces were made again or the window host went away: nothing is in flight and
    /// nothing waits.
    pub fn reset(&mut self) {
        self.in_flight = None;
        self.ready = None;
        self.writing = None;
    }

    fn send_ready(&mut self, now: Instant) -> Option<Announce> {
        if self.in_flight.is_some() {
            return None;
        }
        let (slot, content) = self.ready.take()?;
        let sequence = self.next_sequence;
        self.next_sequence += 1;
        self.in_flight = Some((slot, sequence, now));
        Some(Announce {
            slot,
            sequence,
            content,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn announce(slot: usize, sequence: u64) -> Option<Announce> {
        Some(Announce {
            slot,
            sequence,
            content: (10, 10),
        })
    }

    #[test]
    fn pr3_a_frame_goes_at_once_when_the_window_host_is_ready() {
        let now = Instant::now();
        let mut slots = FrameSlots::new();
        let slot = slots.begin();
        assert_eq!(slots.written(slot, (10, 10), now), announce(0, 1));
    }

    #[test]
    fn pr3_the_renderer_never_writes_the_surface_the_window_host_reads() {
        let now = Instant::now();
        let mut slots = FrameSlots::new();
        let first = slots.begin();
        slots.written(first, (10, 10), now);
        for _ in 0..3 {
            let next = slots.begin();
            assert_ne!(next, first);
            assert_eq!(slots.written(next, (10, 10), now), None);
        }
    }

    #[test]
    fn pr3_only_the_newest_waiting_frame_is_shown_next() {
        let now = Instant::now();
        let mut slots = FrameSlots::new();
        let first = slots.begin();
        slots.written(first, (10, 10), now);
        let second = slots.begin();
        slots.written(second, (20, 20), now);
        let third = slots.begin();
        slots.written(third, (30, 30), now);
        let next = slots.presented(1, now).unwrap();
        assert_eq!((next.slot, next.sequence, next.content), (1, 2, (30, 30)));
        assert_eq!(slots.presented(2, now), None);
    }

    #[test]
    fn pr3_a_frame_being_written_is_not_announced_until_it_is_written() {
        let now = Instant::now();
        let mut slots = FrameSlots::new();
        let first = slots.begin();
        slots.written(first, (10, 10), now);
        let second = slots.begin();
        assert_eq!(slots.presented(1, now), None);
        assert_eq!(slots.written(second, (10, 10), now), announce(1, 2));
    }

    #[test]
    fn nfr5_an_answer_for_another_frame_is_ignored() {
        let now = Instant::now();
        let mut slots = FrameSlots::new();
        let first = slots.begin();
        slots.written(first, (10, 10), now);
        let second = slots.begin();
        slots.written(second, (10, 10), now);
        assert_eq!(slots.presented(7, now), None);
        assert_eq!(slots.presented(1, now), announce(1, 2));
    }

    #[test]
    fn nfr5_a_window_host_that_does_not_answer_is_not_waited_for() {
        let now = Instant::now();
        let mut slots = FrameSlots::new();
        let first = slots.begin();
        slots.written(first, (10, 10), now);
        let second = slots.begin();
        slots.written(second, (10, 10), now);
        assert_eq!(slots.overdue(now), None);
        let later = now + PRESENT_WAIT;
        assert_eq!(slots.overdue(later), announce(1, 2));
    }

    #[test]
    fn pr3_after_a_reset_the_next_frame_goes_at_once() {
        let now = Instant::now();
        let mut slots = FrameSlots::new();
        let first = slots.begin();
        slots.written(first, (10, 10), now);
        slots.reset();
        let next = slots.begin();
        assert_eq!(slots.written(next, (10, 10), now), announce(0, 2));
    }
}
