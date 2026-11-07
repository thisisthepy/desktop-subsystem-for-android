//! How a task window's size follows the frames its app draws, so the window is never shown at
//! a size other than the frame in it.
//!
//! A native window lays itself out within the resize, so its content always fits it. An
//! Android app needs tens to hundreds of milliseconds to draw at a new size, and anything the
//! window showed meanwhile (the last frame stretched, cropped, or with a margin filled in) did
//! not match the window. So the window takes a size only together with a frame of that size:
//!
//! - When the user drags an edge (or anything else proposes a size), the size goes to the
//!   guest, and the window stays at the size of the frame it shows, or takes the size of a
//!   newer frame that has arrived and is waiting ([`FrameFollow::size_for`]).
//! - A frame of the window's size is shown at once. A frame of another size is kept out of
//!   sight, and the window is told to take its size; the frame is shown in the same
//!   composition commit as the new window size ([`FrameFollow::view_taken`]).
//! - Between the window choosing a kept frame's size and taking it, that frame is claimed:
//!   a newer frame waits rather than replace it, so the size taken always has its frame.
//! - A move to a monitor of another scale is the one size the window takes at once, before a
//!   frame of it ([`FrameFollow::scale_changed`]). The desktop suggests that size so the window
//!   keeps its size to the eye, and it decides which monitor the window is on from the window's
//!   rectangle. A window that kept its old pixel size there would be too large or too small on
//!   the new monitor by the ratio of the two scales, its rectangle would fall back on the
//!   monitor it came from, and the desktop would change its scale back and suggest the size
//!   scaled the other way: the window and its task would flap between two sizes for as long
//!   as the window stayed near the edge. Until the app has drawn the new size, the window shows
//!   its last frame 1:1, and frames the app still draws at the old size are neither shown nor
//!   followed.
//!
//! The window therefore follows the mouse as fast as the app draws, and the last size of a
//! drag, which the guest is asked for when the drag ends, arrives as a frame like any other.
//! It names no window system.

/// What to do with a frame that arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    /// It is the window's size: show it now.
    Show,
    /// Another size: keep it out of sight and have the window take its size.
    Keep,
    /// The window is taking the size of the frame kept before it: wait until it has.
    Wait,
    /// It is of the size the window had before its monitor's scale changed: keep it out of
    /// sight and leave the window at the size it took for the new scale.
    Stale,
}

/// One window's frames and size.
#[derive(Debug, Clone, Default)]
pub struct FrameFollow {
    /// The window's client size.
    view: (u32, u32),
    /// The size of the frame on the screen, once one is.
    shown: Option<(u32, u32)>,
    /// A frame that arrived at another size than the window's, not yet shown.
    kept: Option<(u32, u32)>,
    /// Whether the window has chosen the kept frame's size and not yet taken it.
    claimed: bool,
    /// The size of the frame shown when the window's monitor changed scale, until a frame of
    /// another size is shown: frames of it were drawn for the old scale.
    stale: Option<(u32, u32)>,
}

impl FrameFollow {
    /// A window whose client area is `view` pixels and that has shown nothing yet.
    pub fn new(view: (u32, u32)) -> Self {
        FrameFollow {
            view,
            ..Self::default()
        }
    }

    /// The window's client size.
    pub fn view(&self) -> (u32, u32) {
        self.view
    }

    /// The size of the frame on the screen.
    pub fn shown(&self) -> Option<(u32, u32)> {
        self.shown
    }

    /// The size of a frame kept out of sight for the window to take.
    pub fn kept(&self) -> Option<(u32, u32)> {
        self.kept
    }

    /// Whether a frame that arrives now must wait.
    pub fn claimed(&self) -> bool {
        self.claimed
    }

    /// A frame of `size` arrived.
    pub fn frame_arrived(&mut self, size: (u32, u32)) -> Arrival {
        if self.claimed {
            return Arrival::Wait;
        }
        if size == self.view {
            self.shown = Some(size);
            self.kept = None;
            self.stale = None;
            Arrival::Show
        } else if self.stale == Some(size) {
            Arrival::Stale
        } else {
            self.kept = Some(size);
            Arrival::Keep
        }
    }

    /// Something proposes a client size for the window (a step of a drag, a maximise, a move
    /// to another monitor). Returns the size the window takes instead: a kept frame's, which
    /// is claimed until the window has taken it, or the size of the frame it shows. `None`
    /// before any frame has arrived, when the window takes what is proposed.
    ///
    /// After a change of monitor scale, and until the app has drawn for it, the size the
    /// window took for the new scale: the frame on the screen has the old scale's size, and
    /// going back to it would start the flapping [`scale_changed`](Self::scale_changed)
    /// prevents. A proposal of that old size itself (the window gives up on the new one,
    /// because the guest did not take it) is taken, and frames of it are followed again.
    pub fn size_for(&mut self, proposed: (u32, u32)) -> Option<(u32, u32)> {
        if let Some(kept) = self.kept {
            self.claimed = true;
            return Some(kept);
        }
        if let Some(stale) = self.stale {
            if proposed == stale {
                self.stale = None;
                return Some(proposed);
            }
            return Some(self.view);
        }
        self.shown
    }

    /// The window's monitor changed scale, and the desktop suggests a client area of
    /// `proposed` pixels for it there, which keeps it the same size to the eye. Returns the
    /// size the window takes: `proposed` itself, at once, whatever frame there is. A frame
    /// kept for the old scale is let go, and frames of the size shown until now, which the app
    /// may still be drawing, are not followed ([`Arrival::Stale`]) until a frame of another
    /// size is shown.
    pub fn scale_changed(&mut self, proposed: (u32, u32)) -> (u32, u32) {
        self.claimed = false;
        self.kept = None;
        self.stale = self.shown.filter(|shown| *shown != proposed);
        proposed
    }

    /// The window's client area is now `view`. Returns whether the kept frame is of that size
    /// and is to be shown with it. Ends any claim.
    pub fn view_taken(&mut self, view: (u32, u32)) -> bool {
        self.view = view;
        self.claimed = false;
        if self.kept == Some(view) {
            self.shown = self.kept.take();
            self.stale = None;
            true
        } else {
            false
        }
    }

    /// The window settled without taking the size it chose (the window system did not apply
    /// it): frames no longer wait for it.
    pub fn release(&mut self) {
        self.claimed = false;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compositor::window_sizing::{clamp_size, rescale};

    /// A window and its app, stepped by hand: what is on the screen after each step.
    struct Screen {
        follow: FrameFollow,
        /// What the frame on the screen is, as the compositor shows it.
        on_screen: Option<(u32, u32)>,
    }

    impl Screen {
        fn new(view: (u32, u32), first: (u32, u32)) -> Screen {
            let mut screen = Screen {
                follow: FrameFollow::new(view),
                on_screen: None,
            };
            screen.frame(first);
            screen
        }

        /// The window proposes `size`, takes what it is given, and shows the kept frame with
        /// it when that is its size.
        fn propose(&mut self, size: (u32, u32)) {
            let taken = self.follow.size_for(size).unwrap_or(size);
            if self.follow.view_taken(taken) {
                self.on_screen = self.follow.shown();
            }
            self.check();
        }

        /// The app drew a frame of `size`; a kept one has the window follow it at once.
        fn frame(&mut self, size: (u32, u32)) {
            match self.follow.frame_arrived(size) {
                Arrival::Show => self.on_screen = Some(size),
                Arrival::Keep => {
                    // The window is told, and takes the size of the kept frame.
                    let kept = self.follow.kept().expect("a kept frame");
                    self.propose(kept);
                }
                Arrival::Wait => panic!("no claim is open between steps"),
            }
            self.check();
        }

        fn check(&self) {
            if let Some(frame) = self.on_screen {
                assert_eq!(
                    self.follow.view(),
                    frame,
                    "the window is {:?} but shows a frame of {frame:?}",
                    self.follow.view()
                );
            }
        }
    }

    #[test]
    fn fr4_the_window_stays_at_its_frames_size_until_a_frame_of_the_new_size_arrives() {
        let mut screen = Screen::new((800, 600), (800, 600));
        // The drag proposes larger sizes; the app has drawn none of them yet.
        for width in [820, 860, 900] {
            screen.propose((width, 600));
            assert_eq!(screen.follow.view(), (800, 600));
        }
        // The app draws the size it was asked for: the window takes it with the frame.
        screen.frame((860, 600));
        assert_eq!(screen.follow.view(), (860, 600));
        assert_eq!(screen.on_screen, Some((860, 600)));
    }

    #[test]
    fn fr4_the_window_is_never_shown_at_a_size_other_than_its_frame() {
        // A drag out and back, with the app drawing every third size late.
        let mut screen = Screen::new((800, 600), (800, 600));
        let mut drawn = Vec::new();
        for step in 0..60u32 {
            let width = 800 + (step % 30) * 10 - if step >= 30 { (step - 30) * 20 } else { 0 };
            let size = (width.max(400), 600 + step % 7);
            screen.propose(size);
            drawn.push(size);
            if step % 3 == 2 {
                let late = drawn[drawn.len() / 2];
                screen.frame(late);
            }
        }
        screen.check();
    }

    #[test]
    fn fr4_the_last_size_of_a_drag_is_reached_exactly() {
        let mut screen = Screen::new((800, 600), (800, 600));
        for width in (810..=1000).step_by(10) {
            screen.propose((width, 700));
        }
        // The drag ended at 1000x700; the app draws an older size first, then that one.
        screen.frame((900, 700));
        screen.frame((1000, 700));
        assert_eq!(screen.follow.view(), (1000, 700));
        assert_eq!(screen.on_screen, Some((1000, 700)));
    }

    #[test]
    fn fr4_a_newer_frame_waits_while_the_window_takes_the_kept_one() {
        let mut follow = FrameFollow::new((800, 600));
        follow.frame_arrived((800, 600));
        assert_eq!(follow.frame_arrived((900, 600)), Arrival::Keep);
        assert_eq!(follow.size_for((950, 600)), Some((900, 600)));
        // Between choosing the size and taking it, the kept frame is not replaced.
        assert_eq!(follow.frame_arrived((950, 600)), Arrival::Wait);
        assert!(follow.view_taken((900, 600)));
        assert_eq!(follow.shown(), Some((900, 600)));
        assert_eq!(follow.frame_arrived((950, 600)), Arrival::Keep);
    }

    #[test]
    fn fr4_a_size_the_window_system_did_not_apply_lets_frames_go_again() {
        let mut follow = FrameFollow::new((800, 600));
        follow.frame_arrived((800, 600));
        follow.frame_arrived((900, 600));
        follow.size_for((900, 600));
        follow.release();
        assert_eq!(follow.frame_arrived((800, 600)), Arrival::Show);
    }

    #[test]
    fn fr4_before_any_frame_the_window_takes_what_is_proposed() {
        let mut follow = FrameFollow::new((800, 600));
        assert_eq!(follow.size_for((900, 700)), None);
        assert!(!follow.view_taken((900, 700)));
        assert_eq!(follow.frame_arrived((900, 700)), Arrival::Show);
    }

    /// The monitor that holds a point of the desktop, by its scale: a 125% monitor (120 dpi)
    /// left of the 200% primary one (192 dpi), which starts at x = 0.
    fn dpi_at(x: i64) -> u32 {
        if x < 0 {
            120
        } else {
            192
        }
    }

    /// The most the guest draws: its display, which is larger than either monitor.
    const GUEST_MAX: (u32, u32) = (2880, 2880);

    /// A task window the user moves by the middle of its title bar, and the desktop, which
    /// gives the window the scale of the monitor that holds the middle of its rectangle.
    struct Desktop {
        follow: FrameFollow,
        /// The window's left edge.
        left: i64,
        /// The scale the window has now.
        dpi: u32,
        /// Every size the guest was asked for, in order.
        asked: Vec<(u32, u32)>,
        /// How many times the window's scale changed.
        scale_changes: usize,
    }

    impl Desktop {
        fn new(view: (u32, u32), cursor: i64) -> Desktop {
            let mut follow = FrameFollow::new(view);
            assert_eq!(follow.frame_arrived(view), Arrival::Show);
            let mut desktop = Desktop {
                follow,
                left: 0,
                dpi: dpi_at(cursor),
                asked: Vec::new(),
                scale_changes: 0,
            };
            desktop.drag_to(cursor);
            desktop
        }

        fn middle(&self) -> i64 {
            self.left + i64::from(self.follow.view().0) / 2
        }

        /// The user moves the window so the cursor, in the middle of its title bar, is at
        /// `cursor`.
        fn drag_to(&mut self, cursor: i64) {
            self.left = cursor - i64::from(self.follow.view().0) / 2;
            self.settle(cursor);
        }

        /// The desktop gives the window the scale of the monitor it is on, and does so again
        /// whenever the size the window takes puts it on the other one. It suggests the size
        /// that keeps the window the same size to the eye, around the cursor; the window keeps
        /// that within what the guest draws, and the guest is asked for it, as for every size
        /// the window is proposed.
        fn settle(&mut self, cursor: i64) {
            for _ in 0..10 {
                let dpi = dpi_at(self.middle());
                if dpi == self.dpi {
                    return;
                }
                let suggested =
                    clamp_size(rescale(self.follow.view(), self.dpi, dpi), Some(GUEST_MAX));
                self.dpi = dpi;
                self.scale_changes += 1;
                self.asked.push(suggested);
                let taken = self.follow.scale_changed(suggested);
                self.left = cursor - i64::from(suggested.0) / 2;
                self.follow.view_taken(taken);
            }
        }

        /// The app draws a frame of `size`. The window takes the size of a frame it keeps,
        /// with its left edge where it is, and the desktop looks at where that leaves it.
        fn frame(&mut self, size: (u32, u32), cursor: i64) -> Arrival {
            let arrival = self.follow.frame_arrived(size);
            if arrival == Arrival::Keep {
                let kept = self.follow.size_for(size).expect("a kept frame");
                assert!(self.follow.view_taken(kept));
                self.settle(cursor);
            }
            arrival
        }
    }

    #[test]
    fn fr4_a_window_moved_onto_a_monitor_of_another_scale_settles_at_one_size() {
        // Chromium's window, 2637x1691 on the 200% monitor, is moved by its title bar onto the
        // 125% one. A window that kept its old pixel size there, waiting for a frame of the
        // suggested 1648x1057, was too wide for its middle to stay on the 125% monitor: it went
        // back to 200%, was suggested 1.6 times its size (2880x2706 within what the guest
        // draws), was put on the 125% monitor again, and so on. The guest was asked for
        // 1648x1057 and 2880x2706 in turn, and every frame it drew of either moved the window
        // again, up to the 2880x2880 the guest draws at most.
        let mut desktop = Desktop::new((2637, 1691), 200);
        for cursor in (-1000..=100).rev().step_by(100) {
            desktop.drag_to(cursor);
        }
        assert_eq!(desktop.scale_changes, 1, "the scale went back and forth");
        assert_eq!(desktop.asked, vec![(1648, 1057)]);
        assert_eq!(desktop.follow.view(), (1648, 1057));
        // The app still draws at the old size for a moment: that frame does not move the
        // window back. Then it draws the new size, which the window has already.
        assert_eq!(desktop.frame((2637, 1691), -1000), Arrival::Stale);
        assert_eq!(desktop.follow.view(), (1648, 1057));
        assert_eq!(desktop.frame((1648, 1057), -1000), Arrival::Show);
        assert_eq!(desktop.follow.shown(), Some((1648, 1057)));
        assert_eq!(desktop.scale_changes, 1);
        assert_eq!(desktop.asked, vec![(1648, 1057)]);
    }

    #[test]
    fn fr4_a_change_of_scale_is_taken_at_once_and_holds_until_its_frame_comes() {
        let mut follow = FrameFollow::new((2637, 1691));
        follow.frame_arrived((2637, 1691));
        assert_eq!(follow.scale_changed((1648, 1057)), (1648, 1057));
        assert!(!follow.view_taken((1648, 1057)), "no frame of it yet");
        // A move that proposes the window's size again keeps it, rather than the size of the
        // frame on the screen, which was drawn for the old scale.
        assert_eq!(follow.size_for((1648, 1057)), Some((1648, 1057)));
        assert_eq!(follow.frame_arrived((2637, 1691)), Arrival::Stale);
        // A frame the app draws at another size is followed as any other.
        assert_eq!(follow.frame_arrived((1700, 1057)), Arrival::Keep);
        assert_eq!(follow.size_for((1648, 1057)), Some((1700, 1057)));
        assert!(follow.view_taken((1700, 1057)));
        // The old scale's frames count as any other size again once it is shown.
        assert_eq!(follow.frame_arrived((2637, 1691)), Arrival::Keep);
    }

    #[test]
    fn fr4_a_window_that_goes_back_to_its_old_size_follows_its_frames_again() {
        // The guest did not take the size of the new scale, and the window takes the size the
        // task kept: frames of that size are the task's frames again.
        let mut follow = FrameFollow::new((2637, 1691));
        follow.frame_arrived((2637, 1691));
        follow.scale_changed((1648, 1057));
        follow.view_taken((1648, 1057));
        assert_eq!(follow.size_for((2637, 1691)), Some((2637, 1691)));
        follow.view_taken((2637, 1691));
        assert_eq!(follow.frame_arrived((2637, 1691)), Arrival::Show);
    }
}
