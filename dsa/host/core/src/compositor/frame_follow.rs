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
//! - A window that holds the size the desktop gives it (maximised, snapped, full screen; see
//!   [`size_is_held`](super::window_sizing::size_is_held)) takes every size proposed at once
//!   and shows every frame as it comes, in the window's middle if it is of another size
//!   ([`FrameFollow::set_held`]). The guest may keep the task smaller than the window (its
//!   display holds no more), and a window that took that size as its answer would leave the
//!   place the desktop gave it.
//!
//! The window therefore follows the mouse as fast as the app draws, and the last size of a
//! drag, which the guest is asked for when the drag ends, arrives as a frame like any other.
//! It names no window system.

use super::window_sizing::FramePlacement;

/// What to do with a frame that arrived.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrival {
    /// It is the window's size: show it now.
    Show,
    /// Another size: keep it out of sight and have the window take its size.
    Keep,
    /// The window is taking the size of the frame kept before it: wait until it has.
    Wait,
    /// It is of the size the window had before its monitor's scale changed, or of a size the
    /// user dragged past before the size of the frame on the screen: keep it out of sight and
    /// leave the window at the size it has.
    Stale,
}

/// How many sizes proposed for a window are remembered, to tell a late frame of a size dragged
/// past from a new one.
const PROPOSALS_KEPT: usize = 256;

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
    /// Whether the window holds the size the desktop gave it ([`FrameFollow::set_held`]).
    held: bool,
    /// The sizes proposed for the window, oldest first, each once at its latest place.
    proposals: Vec<(u32, u32)>,
    /// While a window that holds its size waits for the guest's answer to a new size: the size
    /// of the frame it showed when the size changed. Frames of that size are the app's old
    /// drawing; the first frame of another size is the guest's answer
    /// ([`FrameFollow::strips_filled`]).
    waiting_from: Option<Option<(u32, u32)>>,
    /// The largest client area the guest can draw for the task, in window pixels, when known
    /// ([`FrameFollow::set_limit`]).
    limit: Option<(u32, u32)>,
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

    /// Whether the window holds the size the desktop gave it.
    pub fn held(&self) -> bool {
        self.held
    }

    /// Whether what the frame leaves of the window is filled with the app's colour, decided
    /// by geometry: the frame where the window shows it (`placement`) does not cover the
    /// window, and it cannot, or no longer will. Only a window that holds its size has such
    /// strips; any other takes its frame's size.
    ///
    /// - A held window larger than the largest client area the guest can draw
    ///   ([`FrameFollow::set_limit`]) can never be covered, whatever frame comes (including
    ///   one of the size already shown, a window already at the limit maximised again), so
    ///   its strips are filled at once.
    /// - Within the limit, the strips beside a settled frame that does not cover the window
    ///   are filled. While the window's new size is pending (the moment of a maximise, a snap
    ///   or a restore), the app is still catching up and its stretched last frame is shown
    ///   with nothing drawn around it.
    pub fn strips_filled(&self, placement: FramePlacement) -> bool {
        if !self.held {
            return false;
        }
        let view = self.view;
        let covers =
            placement.origin == (0, 0) && placement.size.0 >= view.0 && placement.size.1 >= view.1;
        if covers {
            return false;
        }
        let beyond_limit = self
            .limit
            .is_some_and(|limit| view.0 > limit.0 || view.1 > limit.1);
        beyond_limit || !self.pending()
    }

    /// The largest client area the guest can draw for the task at the window's scale: its
    /// display's size, or the largest task size, in window pixels. `None` while not known.
    pub fn set_limit(&mut self, limit: Option<(u32, u32)>) {
        self.limit = limit;
    }

    /// Whether a window that holds its size still waits for the guest's answer to it: the
    /// frame on the screen is the app's drawing of the size before, and is shown stretched to
    /// the window until the answer comes
    /// ([`shown_frame_placement`](super::window_sizing::shown_frame_placement)).
    pub fn pending(&self) -> bool {
        self.held && self.waiting_from.is_some()
    }

    /// The limit [`FrameFollow::set_limit`] gave.
    pub fn limit(&self) -> Option<(u32, u32)> {
        self.limit
    }

    /// A frame of `size` is on the screen of a window that holds its size.
    fn held_frame_shown(&mut self, size: (u32, u32)) {
        if self.waiting_from.is_some_and(|from| from != Some(size)) {
            self.waiting_from = None;
        }
        self.shown = Some(size);
    }

    /// The window holds the size the desktop gives it (`true`: it is maximised, snapped or
    /// full screen) or takes the size of its frames again. While it holds its size, every size
    /// proposed is taken at once and every frame is shown as it comes, whatever its size: the
    /// window shows it in its middle. Nothing is kept or claimed. When the window lets go, it
    /// keeps the size it has until a frame of the size it is proposed next arrives, as it would
    /// after any frame of its size.
    pub fn set_held(&mut self, held: bool) {
        if self.held == held {
            return;
        }
        self.held = held;
        self.kept = None;
        self.claimed = false;
        self.stale = None;
        self.proposals.clear();
        self.waiting_from = held.then_some(self.shown);
        if !held {
            self.shown = Some(self.view);
        }
    }

    /// A frame of `size` arrived.
    pub fn frame_arrived(&mut self, size: (u32, u32)) -> Arrival {
        if self.claimed {
            return Arrival::Wait;
        }
        if self.held {
            self.held_frame_shown(size);
            self.kept = None;
            return Arrival::Show;
        }
        if size == self.view {
            self.shown = Some(size);
            self.kept = None;
            self.stale = None;
            Arrival::Show
        } else if self.stale == Some(size) {
            Arrival::Stale
        } else if self.dragged_past(size) {
            // A late frame of a size the user dragged past: the window has a newer size, whose
            // frame the app drew or is drawing. Following it would shrink or grow the window
            // back to that older size after the user let go. Only Android refusing the size
            // the user let go at moves the window there: the window manager then resizes the
            // window to the task's size, and frames of it are of the window's size.
            Arrival::Stale
        } else {
            self.kept = Some(size);
            Arrival::Keep
        }
    }

    /// Whether a frame of `size` is of a size proposed for the window before the size the
    /// window has now. A window whose size was never proposed (the guest resized it) has no
    /// order to go by, and every frame of another size is followed.
    pub fn dragged_past(&self, size: (u32, u32)) -> bool {
        let at = |wanted: (u32, u32)| self.proposals.iter().rposition(|p| *p == wanted);
        match (at(size), at(self.view)) {
            (Some(frame), Some(view)) => frame < view,
            _ => false,
        }
    }

    /// Remembers `size` as the newest size proposed for the window.
    fn remember(&mut self, size: (u32, u32)) {
        if self.proposals.last() == Some(&size) {
            return;
        }
        self.proposals.retain(|p| *p != size);
        self.proposals.push(size);
        if self.proposals.len() > PROPOSALS_KEPT {
            self.proposals.remove(0);
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
        if self.held {
            return None;
        }
        if let Some(kept) = self.kept {
            self.claimed = true;
            return Some(kept);
        }
        self.remember(proposed);
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
        // Sizes of the old scale say nothing about the order of the new scale's frames.
        self.proposals.clear();
        // A window that holds its size shows every frame anyway.
        self.stale = self.shown.filter(|shown| !self.held && *shown != proposed);
        proposed
    }

    /// The scale the window shows its task at changed, and the newest frame, which the app drew
    /// before, now covers `size` window pixels. Its size changed only because the same frame
    /// is measured at another scale, not because the app drew a new size: it is shown if it
    /// fits the window, and never resizes the window. Followed like a new frame, it would
    /// shrink or grow the window by the ratio of the two scales the moment the task changes
    /// density at the end of a move to another monitor, and the next frame, drawn for the new
    /// density at the window's size, would put it back: a snap after the user let go.
    ///
    /// Further frames of `size` are of the same old drawing and are not followed either, until
    /// a frame of another size is shown ([`Arrival::Stale`]).
    pub fn rescaled(&mut self, size: (u32, u32)) -> Arrival {
        if self.claimed {
            return Arrival::Wait;
        }
        if self.held || size == self.view {
            if self.held {
                self.held_frame_shown(size);
            } else {
                self.shown = Some(size);
            }
            self.kept = None;
            if !self.held {
                self.stale = None;
            }
            return Arrival::Show;
        }
        // A frame kept at the old scale's measure is let go: its size means another now.
        self.kept = None;
        self.stale = Some(size);
        Arrival::Stale
    }

    /// The window's client area is now `view`. Returns whether the kept frame is of that size
    /// and is to be shown with it. Ends any claim.
    pub fn view_taken(&mut self, view: (u32, u32)) -> bool {
        if self.held && view != self.view {
            self.waiting_from = Some(self.shown);
        }
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
    use crate::compositor::window_sizing::{clamp_size, rescale, shown_frame_placement};

    /// Whether the strips beside the frame `follow` shows (1:1, as core places it) are filled.
    fn filled(follow: &FrameFollow) -> bool {
        let frame = follow.shown().unwrap_or(follow.view());
        let placement = shown_frame_placement(
            frame,
            follow.view(),
            (1, 1),
            follow.held(),
            follow.pending(),
        );
        follow.strips_filled(placement)
    }

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
                // A frame of the size before a scale change: out of sight, and the window
                // keeps the size it took.
                Arrival::Stale => {}
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
    fn fr4_a_maximised_window_keeps_its_size_when_the_guest_keeps_the_task_smaller() {
        // A window of 1401x1649 on a 125% monitor shows a task at the 200% main monitor's
        // density. Maximised, it is proposed 2560x1380; the guest's display holds 2880 task
        // pixels across, so the task's frame comes back 1802x1380 in window pixels.
        let mut follow = FrameFollow::new((1401, 1649));
        assert_eq!(follow.frame_arrived((1401, 1649)), Arrival::Show);
        follow.set_held(true);
        assert!(follow.held());
        // The window takes the maximised size at once, not the size of its last frame.
        assert_eq!(follow.size_for((2560, 1380)), None);
        assert!(!follow.view_taken((2560, 1380)));
        // The old frame and the capped one are shown in it; neither is kept for the window to
        // take, so the window never goes back to either size.
        assert_eq!(follow.frame_arrived((1401, 1649)), Arrival::Show);
        assert_eq!(follow.frame_arrived((1802, 1380)), Arrival::Show);
        assert_eq!(follow.kept(), None);
        assert_eq!(follow.view(), (2560, 1380));
        // Another size the desktop gives it (snapped to the other half, say) is taken as well.
        assert_eq!(follow.size_for((1280, 1380)), None);
        // A change of monitor scale leaves no frame stale while it holds its size.
        follow.scale_changed((2000, 1100));
        assert_eq!(follow.frame_arrived((1802, 1380)), Arrival::Show);

        // Restored, the window follows its frames again: it stays at its size until the app
        // draws the size it is proposed, then takes that.
        let mut follow = FrameFollow::new((2560, 1380));
        follow.set_held(true);
        follow.frame_arrived((1802, 1380));
        follow.set_held(false);
        assert_eq!(follow.size_for((1401, 1649)), Some((2560, 1380)));
        assert_eq!(follow.frame_arrived((1401, 1649)), Arrival::Keep);
        assert_eq!(follow.size_for((1401, 1649)), Some((1401, 1649)));
        assert!(follow.view_taken((1401, 1649)));
    }

    #[test]
    fn fr4_a_maximise_on_the_main_monitor_shows_the_last_frame_as_before_with_nothing_filled() {
        // Settings at 1041x1495 is maximised on the 2880x1800 main monitor (work area
        // 2880x1704). Until the app draws the new size, the last frame is shown as it was before
        // the strips beside it were ever filled: no solid colour is drawn around it.
        let mut follow = FrameFollow::new((1041, 1495));
        follow.frame_arrived((1041, 1495));
        follow.set_held(true);
        assert!(!filled(&follow));
        assert_eq!(follow.size_for((2880, 1704)), None);
        follow.view_taken((2880, 1704));
        assert!(!filled(&follow));
        // The app draws its old size once more: still the old drawing, still nothing filled.
        assert_eq!(follow.frame_arrived((1041, 1495)), Arrival::Show);
        assert!(!filled(&follow));
        // The frame of the maximised size covers the window; nothing is left to fill.
        assert_eq!(follow.frame_arrived((2880, 1704)), Arrival::Show);
        // Restored, the window follows its frames and never fills anything.
        follow.set_held(false);
        assert!(!filled(&follow));
        // A window that never holds its size never fills anything either.
        let mut follow = FrameFollow::new((800, 600));
        follow.frame_arrived((800, 600));
        follow.size_for((900, 600));
        follow.frame_arrived((900, 600));
        assert!(!filled(&follow));
    }

    #[test]
    fn fr4_a_size_limited_maximise_on_a_secondary_monitor_leaves_no_transparent_strip() {
        // A task kept at the 200% main monitor's density is maximised on a 2560x1440 monitor
        // at 125%: the window is 2560x1380, the guest's display holds 3840 task pixels across,
        // and the task comes back 3840x2205, shown 2400x1380 in the window. The strips beside
        // it would stay for as long as the window is maximised, so they are filled.
        let mut follow = FrameFollow::new((1140, 836));
        follow.frame_arrived((1140, 836));
        follow.set_held(true);
        follow.view_taken((2560, 1380));
        assert!(!filled(&follow));
        assert_eq!(follow.frame_arrived((2400, 1380)), Arrival::Show);
        assert!(filled(&follow));
        // Snapped to another size, the window waits for the answer to it again.
        follow.view_taken((1280, 1380));
        assert!(!filled(&follow));
        assert_eq!(follow.frame_arrived((2400, 1380)), Arrival::Show);
        assert!(!filled(&follow));
        assert_eq!(follow.frame_arrived((1200, 1380)), Arrival::Show);
        assert!(filled(&follow));
    }

    #[test]
    fn fr4_a_window_already_at_the_limit_maximised_again_leaves_no_transparent_strip() {
        // A task at the 200% main monitor's density on a 2560x1440 monitor at 125%: the guest
        // can draw at most 2400x1380 window pixels for it. Snapped to that limit, then
        // maximised to 2560x1380, the guest answers with the same 2400x1380 frame.
        let mut follow = FrameFollow::new((2400, 1380));
        follow.set_limit(Some((2400, 1380)));
        follow.frame_arrived((2400, 1380));
        follow.set_held(true);
        follow.view_taken((2560, 1380));
        // Beyond the limit, the strips are filled at once and stay filled.
        assert!(filled(&follow));
        assert_eq!(follow.frame_arrived((2400, 1380)), Arrival::Show);
        assert!(filled(&follow));
        // Restored and maximised again: the same.
        follow.set_held(false);
        assert!(!filled(&follow));
        follow.set_held(true);
        follow.view_taken((2560, 1380));
        assert!(filled(&follow));

        // On the main monitor the limit is above the maximised size, and a maximise the app
        // catches up with fills nothing meanwhile, as before.
        let mut follow = FrameFollow::new((1041, 1495));
        follow.set_limit(Some((2880, 2880)));
        follow.frame_arrived((1041, 1495));
        follow.set_held(true);
        follow.view_taken((2880, 1704));
        assert!(!filled(&follow));
    }

    #[test]
    fn fr4_a_maximised_window_stays_maximised_when_frames_of_its_size_or_its_restored_size_arrive()
    {
        // Settings at 1041x1495 is maximised on a 2880x1800 monitor whose work area is
        // 2880x1704: the window's client area becomes the work area at once.
        let mut follow = FrameFollow::new((1041, 1495));
        assert_eq!(follow.frame_arrived((1041, 1495)), Arrival::Show);
        follow.set_held(true);
        assert_eq!(follow.size_for((2880, 1704)), None);
        assert!(!follow.view_taken((2880, 1704)));
        assert_eq!(follow.view(), (2880, 1704));
        // Frames the app still draws at the restored size are shown in it, and none is kept
        // for the window to take: a kept frame is what would move it back to that size.
        assert_eq!(follow.frame_arrived((1041, 1495)), Arrival::Show);
        assert_eq!(follow.kept(), None);
        assert_eq!(follow.view(), (2880, 1704));
        // The frame of the maximised size fills it.
        assert_eq!(follow.frame_arrived((2880, 1704)), Arrival::Show);
        assert_eq!(follow.shown(), Some((2880, 1704)));
        assert_eq!(follow.view(), (2880, 1704));
        // Windows proposes the maximised size again (a change of frame or of the work area):
        // the window takes it as it is, and nothing claims a size for it.
        assert_eq!(follow.size_for((2880, 1704)), None);
        assert!(!follow.claimed());
        // A frame of the restored size that the guest sends on its own is shown in it too.
        assert_eq!(follow.frame_arrived((1041, 1495)), Arrival::Show);
        assert_eq!(follow.kept(), None);
        assert_eq!(follow.size_for((2880, 1704)), None);
        assert!(!follow.view_taken((2880, 1704)));
        assert_eq!(follow.view(), (2880, 1704));
    }

    #[test]
    fn fr4_a_dragged_window_takes_the_size_the_guest_keeps_the_task_at() {
        // A drag past what the guest's display holds: the window is never held, so it keeps the
        // size of the largest frame the guest draws rather than show it with a margin.
        let mut screen = Screen::new((1800, 1200), (1800, 1200));
        screen.propose((2400, 1200));
        assert_eq!(screen.follow.view(), (1800, 1200));
        // The guest gives the task the most its display holds.
        screen.frame((2000, 1200));
        assert_eq!(screen.follow.view(), (2000, 1200));
        assert_eq!(screen.on_screen, Some((2000, 1200)));
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
    fn fr4_a_frame_measured_at_a_new_scale_never_resizes_the_window_200_100_200() {
        // An app-way window of 1186x1711 on a 200% monitor is dragged onto a 100% one and,
        // in a later drag, back. Each time the desktop gives it its new size at once, and the
        // task's last frame, drawn at the old density, is measured at the window's new scale
        // until the task takes the new density. Measured: that frame (740x1058 at 1:1) was
        // followed when the scale went back to 1:1 at the end of the move, and the window shrank
        // to it after the user let go, then grew back with the next frame.
        let mut screen = Screen::new((1186, 1711), (1186, 1711));
        let mut sizes = vec![screen.follow.view()];
        for (suggested, old_frame_now, new_frame) in [
            // 200% to 100%: the old frame shows scaled to 593x856, then at 1:1 as 1186x1711.
            ((595, 857), [(593, 856), (1186, 1711)], (595, 857)),
            // 100% back to 200%: the old frame shows scaled, then at 1:1 as itself.
            ((1188, 1712), [(1190, 1714), (595, 857)], (1188, 1712)),
        ] {
            let taken = screen.follow.scale_changed(suggested);
            screen.follow.view_taken(taken);
            sizes.push(screen.follow.view());
            // The scale changes under the frame on the screen, twice: to the move's interim
            // scale, and back to 1:1 once the task has the new density.
            for measured in old_frame_now {
                assert_ne!(screen.follow.rescaled(measured), Arrival::Keep);
                sizes.push(screen.follow.view());
                // The app still sends frames of that drawing: not followed either.
                assert_eq!(screen.follow.frame_arrived(measured), Arrival::Stale);
                sizes.push(screen.follow.view());
            }
            // The task's first frame at the new density is of the window's size.
            screen.frame(new_frame);
            assert_eq!(screen.on_screen, Some(new_frame));
            sizes.push(screen.follow.view());
        }
        sizes.dedup();
        assert_eq!(
            sizes,
            vec![(1186, 1711), (595, 857), (1188, 1712)],
            "the window took only the sizes the desktop gave it"
        );
    }

    #[test]
    fn fr4_a_frame_that_fits_the_window_at_its_new_scale_is_shown() {
        let mut follow = FrameFollow::new((800, 600));
        follow.frame_arrived((800, 600));
        follow.scale_changed((1600, 1200));
        follow.view_taken((1600, 1200));
        assert_eq!(follow.rescaled((1600, 1200)), Arrival::Show);
        assert_eq!(follow.shown(), Some((1600, 1200)));
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

    #[test]
    fn fr4_a_late_frame_of_a_size_dragged_past_does_not_snap_the_window_back() {
        // FLO, dragged to 677x987: the app draws 670x977, 676x986 and 677x987, and then, after
        // the user let go, more frames of 670x977, a size the drag passed.
        let mut screen = Screen::new((644, 949), (644, 949));
        for size in [(670, 977), (675, 985), (676, 986)] {
            screen.propose(size);
        }
        screen.frame((670, 977));
        assert_eq!(screen.follow.view(), (670, 977));
        screen.propose((677, 987));
        screen.frame((676, 986));
        screen.propose((677, 987));
        screen.frame((677, 987));
        assert_eq!(screen.follow.view(), (677, 987));
        for _ in 0..4 {
            assert_eq!(screen.follow.frame_arrived((670, 977)), Arrival::Stale);
        }
        assert_eq!(screen.follow.view(), (677, 987));
        assert_eq!(screen.follow.kept(), None);
        assert_eq!(screen.on_screen, Some((677, 987)));
        // A frame of the window's size goes on being shown.
        assert_eq!(screen.follow.frame_arrived((677, 987)), Arrival::Show);
    }

    #[test]
    fn fr4_frames_of_a_size_dragged_past_move_the_window_only_once_android_refused_the_size() {
        let mut follow = FrameFollow::new((644, 949));
        follow.frame_arrived((644, 949));
        follow.size_for((670, 977));
        follow.size_for((677, 987));
        assert_eq!(follow.frame_arrived((677, 987)), Arrival::Keep);
        follow.size_for((677, 987));
        assert!(follow.view_taken((677, 987)));
        // However many late frames come, the window keeps the size the user let go at.
        for _ in 0..200 {
            assert_eq!(follow.frame_arrived((670, 977)), Arrival::Stale);
        }
        assert_eq!(follow.view(), (677, 987));
        // Android kept the task at 670x977 although asked again: the window manager resizes
        // the window to it, and its frames are shown.
        assert!(!follow.view_taken((670, 977)));
        assert_eq!(follow.frame_arrived((670, 977)), Arrival::Show);
    }

    #[test]
    fn fr4_a_frame_of_a_size_proposed_after_the_windows_is_followed() {
        // A drag that goes on: the app is behind, and its frames of newer sizes are followed.
        let mut follow = FrameFollow::new((644, 949));
        follow.frame_arrived((644, 949));
        follow.size_for((660, 960));
        follow.size_for((670, 977));
        assert_eq!(follow.frame_arrived((660, 960)), Arrival::Keep);
        follow.size_for((670, 977));
        assert!(follow.view_taken((660, 960)));
        assert_eq!(follow.frame_arrived((670, 977)), Arrival::Keep);
        follow.size_for((670, 977));
        assert!(follow.view_taken((670, 977)));
        // A size the drag comes back to is newer again.
        follow.size_for((660, 960));
        assert_eq!(follow.frame_arrived((660, 960)), Arrival::Keep);
    }
}
