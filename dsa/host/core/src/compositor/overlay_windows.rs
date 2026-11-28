//! Where an app's overlay windows go on the desktop.
//!
//! An app overlay window (floating lyrics, a chat head) is a window an app adds over other
//! apps in Android. The guest reports it like a task ([`crate::tasks::OverlayWindow`]) and the
//! host shows it in a borderless window of its own that stays above the others and has no
//! close button: its app decides when it goes. This module holds the rules of such a window
//! that name no window system; the platform's window host carries them out.
//!
//! **One plane per app.** All of an app's overlays are placed with one translation of the
//! guest's display onto the desktop ([`OverlayPlane`]), at the scale their window shows them
//! at. So two overlays of one app keep the places the app gave them relative to each other: a
//! floating button and the panel it opens beside itself stay side by side wherever the user
//! put the button.
//!
//! **Who moves it.** Only its app. The app moves an overlay by giving it a new place on the
//! guest's display (most overlays are dragged by a finger or the mouse inside the overlay
//! itself); the window then moves by the same distance at its scale. DSA adds no handle and no
//! drag of its own: every press inside the overlay is the app's, and the overlay moves the way
//! its app lets the user move it.
//!
//! **A drag goes onto any monitor** ([`OverlayDrag`]). Android keeps an overlay inside its
//! display, and the plane maps that display onto the desktop at the scale of one monitor, so an
//! app's drag alone could never take the window past the area the display covers there, which
//! is part of one monitor. While the user presses an overlay and its app drags it along with
//! the pointer, the window follows the pointer itself, past where Android stops the overlay
//! and onto any monitor, at that monitor's density. The app keeps hearing the pointer as it
//! did when the press began, so its own drag goes on unchanged. When the press ends the plane
//! is put where the drag left the window, so a later move by the app starts from there. A
//! press the app does not move the overlay with moves nothing.
//!
//! **Where it first goes.** On the monitor of its app's window, or the pointer's when the app
//! has none, where it lies on the guest's display: an overlay at the display's right edge is at
//! the work area's right edge ([`first_place`]).
//!
//! **Remembered per app.** The plane is kept for the app ([`remember`], [`remembered`]) when a
//! drag ends, so an overlay that its app shows again at the same place on the guest's display
//! comes back where the user's drag left it, on that monitor.
//!
//! **Its size** is what the app gave it, at the density of the monitor it is on, and never
//! more than that monitor's work area: a larger one is shown smaller, in proportion
//! ([`shown_size`]). The window manager works out the scale from the density the overlay is
//! drawn at: the guest display's, or, for an app set to System (forced) scaling, the density
//! its task was given, which its process draws its overlays at too, wherever they are. The
//! window's size, its plane, the pointer inside it and a drag across monitors all take that
//! one scale.
//!
//! **Its density**, for an app set to System (forced) scaling: on a monitor whose density
//! differs from its task's, the window manager asks the guest to draw that window alone at the
//! monitor's density, once the user's drag ends there (or the window opens there, or the task
//! takes another density), and at the task's again back on a monitor of the task's density
//! ([`super::task_windows::overlay_density_wanted`]). Until the guest says the app has drawn it
//! so, the window scales the overlay as above; then it shows it 1:1, sharp. The window host
//! only says when a drag of the user's begins and ends.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::task_windows::fit_within;
use super::window_hosts::is_valid_package;
use super::window_sizing::{to_task_position, to_window_position, Scale};

/// A rectangle on the desktop, in its pixels: left, top, right, bottom.
pub type Area = (i32, i32, i32, i32);

/// One point of an app's plane: the place on the guest's display (`guest`, in its pixels) that
/// lies at `host` on the desktop (in desktop pixels).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Anchor {
    pub guest: (i32, i32),
    pub host: (i32, i32),
}

/// How the overlays of one app lie on the desktop: the guest's display moved so that
/// [`Anchor::guest`] lies at [`Anchor::host`], at each window's scale.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct OverlayPlane {
    anchor: Option<Anchor>,
}

impl OverlayPlane {
    /// A plane that starts from what was remembered for the app, if anything.
    pub fn new(remembered: Option<Anchor>) -> OverlayPlane {
        OverlayPlane { anchor: remembered }
    }

    /// The plane's anchor, once an overlay has been placed or something was remembered.
    pub fn anchor(&self) -> Option<Anchor> {
        self.anchor
    }

    /// Where the window of an overlay goes: the top left corner of its client area on the
    /// desktop. `guest` is the overlay's top left corner on the guest's display and `scale`
    /// how large the window shows each guest pixel. The first overlay placed on a plane with
    /// nothing remembered puts the plane where `first` says (see [`first_place`]).
    pub fn place(
        &mut self,
        guest: (i32, i32),
        scale: Scale,
        first: impl FnOnce() -> (i32, i32),
    ) -> (i32, i32) {
        let anchor = *self.anchor.get_or_insert_with(|| Anchor {
            guest,
            host: first(),
        });
        let along = |guest: i32, from: i32, host: i32| -> i32 {
            let moved = to_window_position(f64::from(guest) - f64::from(from), scale);
            (f64::from(host) + moved).round() as i32
        };
        (
            along(guest.0, anchor.guest.0, anchor.host.0),
            along(guest.1, anchor.guest.1, anchor.host.1),
        )
    }

    /// The window of the overlay at `guest` on the guest's display is at `host` now: a drag
    /// carried it there ([`OverlayDrag`]), or it was brought back onto a monitor. The plane
    /// moves with it, and so does every other overlay of the app. Returns the anchor to
    /// remember.
    pub fn moved_to(&mut self, guest: (i32, i32), host: (i32, i32)) -> Anchor {
        let anchor = Anchor { guest, host };
        self.anchor = Some(anchor);
        anchor
    }
}

/// Where an overlay first goes on a monitor's work area `work`: where it lies on the guest's
/// display, edge to edge. `guest` is its top left corner there, `guest_size` its size and
/// `display` the display's size, all in guest pixels; `window` is the size of its window in
/// desktop pixels. An overlay at the display's left edge is at the work area's left edge, one
/// at its right edge at the right edge, and one in between in proportion. With no display
/// size, or an overlay as large as the display, it is centred.
pub fn first_place(
    guest: (i32, i32),
    guest_size: (u32, u32),
    display: (u32, u32),
    window: (u32, u32),
    work: Area,
) -> (i32, i32) {
    let along = |at: i32, size: u32, display: u32, window: u32, start: i32, end: i32| -> i32 {
        let room = i64::from(end) - i64::from(start) - i64::from(window);
        let travel = i64::from(display) - i64::from(size);
        if room <= 0 {
            return start;
        }
        if display == 0 || travel <= 0 {
            return start + (room / 2) as i32;
        }
        let at = i64::from(at).clamp(0, travel);
        start + ((at * room + travel / 2) / travel) as i32
    };
    (
        along(guest.0, guest_size.0, display.0, window.0, work.0, work.2),
        along(guest.1, guest_size.1, display.1, window.1, work.1, work.3),
    )
}

/// The size an overlay's window has on a monitor: `scaled`, the overlay's size at the
/// monitor's density, or the largest size of that shape the work area holds when it is larger.
/// An app sizes an overlay for Android's display, which is as long as the user's longest
/// monitor, so on a smaller monitor a display-wide overlay would reach past the screen.
pub fn shown_size(scaled: (u32, u32), work: Area) -> (u32, u32) {
    let room = (
        (i64::from(work.2) - i64::from(work.0)).max(1) as u32,
        (i64::from(work.3) - i64::from(work.1)).max(1) as u32,
    );
    fit_within(scaled, room)
}

/// `host` moved as little as it takes to keep a window of `size` on the work area `work`:
/// whole when it fits, else with its top left corner on it. An overlay remembered on a
/// monitor that is gone, or that its app moved off the edge of Android's display, is still in
/// reach.
pub fn kept_on(host: (i32, i32), size: (u32, u32), work: Area) -> (i32, i32) {
    let along = |at: i32, size: u32, start: i32, end: i32| -> i32 {
        let last = (i64::from(end) - i64::from(size)).max(i64::from(start));
        i64::from(at).clamp(i64::from(start), last) as i32
    };
    (
        along(host.0, size.0, work.0, work.2),
        along(host.1, size.1, work.1, work.3),
    )
}

/// How far, at 100%, the pointer must push an overlay that lies against an edge of Android's
/// display outward before the window follows it: past the slop of an ordinary press, so a tap
/// or a small slip on an overlay at the edge moves nothing.
pub const PUSH_SLOP_DIP: u32 = 8;

/// Which edge of Android's display an overlay lies against on one axis, as a press finds it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Edge {
    /// Against neither edge, or against both (it spans the display on that axis).
    Neither,
    /// Against the left or top edge.
    Start,
    /// Against the right or bottom edge.
    End,
}

impl Edge {
    /// The edge an overlay at `at` of `size` lies against on a display `display` long. Android
    /// keeps an overlay a little way in at times (an app's own margin), so within a 64th of
    /// the display counts as against it.
    fn of(at: i32, size: u32, display: u32) -> Edge {
        if display == 0 {
            return Edge::Neither;
        }
        let near = i64::from((display / 64).max(2));
        let (at, end) = (i64::from(at), i64::from(at) + i64::from(size));
        match (at <= near, end >= i64::from(display) - near) {
            (true, false) => Edge::Start,
            (false, true) => Edge::End,
            _ => Edge::Neither,
        }
    }
}

/// How the window turns a point of its client area into the point it gives the app while a
/// press lasts: `point * scale + offset`, in window pixels. See [`OverlayDrag::input`].
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct InputRemap {
    pub scale: f64,
    pub offset: (f64, f64),
}

impl InputRemap {
    /// Leaves every point where it is.
    pub const SAME: InputRemap = InputRemap {
        scale: 1.0,
        offset: (0.0, 0.0),
    };

    pub fn apply(&self, (x, y): (f64, f64)) -> (f64, f64) {
        (
            x * self.scale + self.offset.0,
            y * self.scale + self.offset.1,
        )
    }
}

/// One press on an overlay's window, from the moment the button or the finger goes down until
/// it comes up: how an app's own drag of its overlay carries the window onto any monitor.
///
/// **Why.** Android keeps an overlay inside its display, and the app's plane maps the display
/// onto the desktop at one monitor's scale ([`OverlayPlane::place`]). The window can then only
/// go where some place on the display maps to, which is part of one monitor (a 3840 pixel
/// display at 262 dpi covers 1407 pixels of a monitor at 100%), so the app's drag stopped at
/// that edge. Mapping the display onto all monitors at once was the alternative; the display
/// is not large enough to cover them at their densities, and an overlay on a monitor of
/// another density would be the wrong size.
///
/// **How.** Each axis on which the app drags the overlay along with the pointer (it moved the
/// overlay the way the pointer went since the press began) is taken over by the window: from
/// then on it keeps the point of the overlay that was under the pointer under it, wherever the
/// pointer goes, onto any monitor and past where Android stops the overlay. An overlay that
/// already lies against an edge of the display cannot follow the pointer outward, so pushing
/// the pointer outward past [`PUSH_SLOP_DIP`] takes that axis over too. An axis the app does
/// not move with the pointer (a seek bar, a button, an overlay its app moves only up and down)
/// stays where the plane puts it. The app keeps hearing the pointer as the press found the
/// plane ([`Self::input`]), so its drag sees the pointer travel as it did in Android, and the
/// window following the pointer never feeds back into where the app thinks it is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct OverlayDrag {
    /// Where the pointer and the window's client area were on the desktop when the press
    /// began, where the overlay was on the guest's display, and the window's scale then.
    pointer_at_press: (i32, i32),
    window_at_press: (i32, i32),
    guest_at_press: (i32, i32),
    scale_at_press: Scale,
    /// The display edge the overlay lay against on each axis when the press began.
    edges: [Edge; 2],
    /// Where the pointer is now.
    pointer: (i32, i32),
    /// On each axis the window has taken over, how far into the overlay the pointer is, in
    /// the guest's pixels.
    grab: [Option<f64>; 2],
}

/// The two axes of a point.
fn axis(point: (i32, i32), which: usize) -> i32 {
    if which == 0 {
        point.0
    } else {
        point.1
    }
}

impl OverlayDrag {
    /// A press begins with the pointer at `pointer` on the desktop, on the window whose client
    /// area is at `window` and shows each guest pixel at `scale`. The overlay is at `guest` on
    /// the guest's display, `guest_size` large, on a display `display` large.
    pub fn press(
        pointer: (i32, i32),
        window: (i32, i32),
        scale: Scale,
        guest: (i32, i32),
        guest_size: (u32, u32),
        display: (u32, u32),
    ) -> OverlayDrag {
        OverlayDrag {
            pointer_at_press: pointer,
            window_at_press: window,
            guest_at_press: guest,
            scale_at_press: scale,
            edges: [
                Edge::of(guest.0, guest_size.0, display.0),
                Edge::of(guest.1, guest_size.1, display.1),
            ],
            pointer,
            grab: [None, None],
        }
    }

    /// Whether the window has taken over an axis, so the end of the press puts the plane
    /// where the drag left the window.
    pub fn carried(&self) -> bool {
        self.grab.iter().any(Option::is_some)
    }

    /// Takes axis `which` over, with the window's client area at `window` at `scale`.
    fn take(&mut self, which: usize, window: (i32, i32), scale: Scale) {
        let into = f64::from(axis(self.pointer, which)) - f64::from(axis(window, which));
        self.grab[which] = Some(to_task_position(into, scale));
    }

    /// The pointer is at `pointer` now, with the window's client area at `window` at `scale`.
    /// `slop` is [`PUSH_SLOP_DIP`] in the pixels of the window's monitor.
    pub fn pointer_moved(
        &mut self,
        pointer: (i32, i32),
        window: (i32, i32),
        scale: Scale,
        slop: i32,
    ) {
        self.pointer = pointer;
        for which in 0..2 {
            if self.grab[which].is_some() {
                continue;
            }
            let pushed = axis(pointer, which) - axis(self.pointer_at_press, which);
            let outward = match self.edges[which] {
                Edge::Start => pushed < -slop,
                Edge::End => pushed > slop,
                Edge::Neither => false,
            };
            if outward {
                self.take(which, window, scale);
            }
        }
    }

    /// The app moved the overlay to `guest` on the guest's display, which the plane puts at
    /// `placed` on the desktop at `scale`. An axis on which it went the way the pointer went
    /// since the press began is the app dragging it: the window takes that axis over.
    pub fn app_moved(&mut self, guest: (i32, i32), placed: (i32, i32), scale: Scale) {
        for which in 0..2 {
            if self.grab[which].is_some() {
                continue;
            }
            let moved = axis(guest, which) - axis(self.guest_at_press, which);
            let pointed = axis(self.pointer, which) - axis(self.pointer_at_press, which);
            if moved != 0 && moved.signum() == pointed.signum() {
                self.take(which, placed, scale);
            }
        }
    }

    /// Where the window's client area goes on the desktop: on each axis taken over, where it
    /// keeps the point of the overlay that was under the pointer under it at `scale`, and on
    /// the others at `placed`, where the plane puts it.
    pub fn window_at(&self, placed: (i32, i32), scale: Scale) -> (i32, i32) {
        let along = |which: usize| -> i32 {
            match self.grab[which] {
                Some(grab) => {
                    let into = to_window_position(grab, scale).round() as i32;
                    axis(self.pointer, which) - into
                }
                None => axis(placed, which),
            }
        };
        (along(0), along(1))
    }

    /// How the window turns a point of its client area into the point it gives the app, with
    /// its client area at `window` on the desktop at `scale` and the overlay last reported at
    /// `guest`. The app hears the pointer where the plane was when the press began: a pointer
    /// at `P` on the desktop is at `guest_at_press + (P - window_at_press) / scale_at_press` on
    /// the guest's display, however the window has moved or rescaled since. The window gives
    /// points relative to the overlay at its current scale, which the guest adds the overlay's
    /// place to, so this is that point less `guest`, at `scale`. While the window is where the
    /// plane puts it at the press's scale this leaves every point where it is.
    pub fn input(&self, window: (i32, i32), guest: (i32, i32), scale: Scale) -> InputRemap {
        let ratio = to_window_position(1.0, scale) / to_window_position(1.0, self.scale_at_press);
        let along = |which: usize| -> f64 {
            let moved =
                f64::from(axis(window, which)) - f64::from(axis(self.window_at_press, which));
            let behind =
                f64::from(axis(self.guest_at_press, which)) - f64::from(axis(guest, which));
            ratio * moved + to_window_position(behind, scale)
        };
        InputRemap {
            scale: ratio,
            offset: (along(0), along(1)),
        }
    }
}

/// Whether a click activates an overlay's window, giving it the keyboard focus: only for an
/// overlay that may take the focus in Android (its app left `FLAG_NOT_FOCUSABLE` off), and
/// then only by the user's click, never when the window opens or its app changes it. A click
/// on any other overlay goes to the app and leaves the user's own window active.
pub fn click_activates(focusable: bool) -> bool {
    focusable
}

/// The file an app's plane is remembered in, in `dir`; `None` for a name that is no package.
fn place_file(dir: &Path, package: &str) -> Option<PathBuf> {
    is_valid_package(package).then(|| dir.join(format!("{package}.overlay.json")))
}

/// The plane remembered for `package` in `dir`, if one was and it can be read.
pub fn remembered(dir: &Path, package: &str) -> Option<Anchor> {
    let path = place_file(dir, package)?;
    crate::settings::read_json::<Anchor>(&path).ok().flatten()
}

/// Remembers `anchor` as the plane of `package`, in `dir`.
pub fn remember(dir: &Path, package: &str, anchor: Anchor) -> Result<(), String> {
    let path = place_file(dir, package).ok_or_else(|| {
        format!("{package} is not a package name; its overlays are not remembered")
    })?;
    crate::settings::write_json(&path, &anchor)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WORK: Area = (0, 0, 2560, 1400);
    const DISPLAY: (u32, u32) = (3840, 3840);

    #[test]
    fn fr41_an_overlay_first_opens_where_it_lies_on_androids_display() {
        // Floating lyrics at (158, 82) of a 3840 pixel display, shown 318 by 86 on the monitor.
        let at = first_place((158, 82), (508, 138), DISPLAY, (318, 86), WORK);
        assert_eq!(at, (106, 29));
        // Edge to edge: flush left stays flush left, flush right and bottom stay flush.
        assert_eq!(
            first_place((0, 0), (508, 138), DISPLAY, (318, 86), WORK),
            (0, 0)
        );
        assert_eq!(
            first_place((3332, 3702), (508, 138), DISPLAY, (318, 86), WORK),
            (2560 - 318, 1400 - 86)
        );
        // On a monitor that does not start at the desktop's origin.
        let second = (2560, -200, 4480, 880);
        assert_eq!(
            first_place((3332, 0), (508, 138), DISPLAY, (318, 86), second),
            (4480 - 318, -200)
        );
        // A place off the display is taken as the nearest edge.
        assert_eq!(
            first_place((-50, 9000), (508, 138), DISPLAY, (318, 86), WORK),
            (0, 1314)
        );
        // Without the display's size, in the middle.
        assert_eq!(
            first_place((158, 82), (508, 138), (0, 0), (318, 86), WORK),
            (1121, 657)
        );
    }

    #[test]
    fn fr41_a_move_by_the_app_moves_the_window_the_same_distance_at_its_scale() {
        let mut plane = OverlayPlane::new(None);
        let first = plane.place((158, 82), (1, 1), || (106, 29));
        assert_eq!(first, (106, 29));
        // The app drags it 100 right and 40 down: so does the window, pixel for pixel.
        assert_eq!(
            plane.place((258, 122), (1, 1), || unreachable!()),
            (206, 69)
        );
        // On a monitor at half the density, half the distance.
        let mut plane = OverlayPlane::new(None);
        plane.place((158, 82), (1, 2), || (106, 29));
        assert_eq!(
            plane.place((258, 122), (1, 2), || unreachable!()),
            (156, 49)
        );
    }

    #[test]
    fn fr41_a_user_move_and_an_app_move_add_up() {
        let mut plane = OverlayPlane::new(None);
        plane.place((158, 82), (1, 1), || (106, 29));
        // The user's drag left the window at (900, 600).
        plane.moved_to((158, 82), (900, 600));
        assert_eq!(
            plane.place((158, 82), (1, 1), || unreachable!()),
            (900, 600)
        );
        // The app then moves it 30 left: from where the user left it, not from where it was.
        assert_eq!(
            plane.place((128, 82), (1, 1), || unreachable!()),
            (870, 600)
        );
        // A drag leaves it elsewhere while the app has it there, and the app moves it back to
        // its first place: 30 right of where the drag left it.
        plane.moved_to((128, 82), (300, 300));
        assert_eq!(
            plane.place((158, 82), (1, 1), || unreachable!()),
            (330, 300)
        );
    }

    #[test]
    fn fr41_overlays_of_one_app_keep_their_places_relative_to_each_other() {
        let mut plane = OverlayPlane::new(None);
        // A floating button, which a drag carries elsewhere.
        plane.place((3600, 1800), (1, 1), || (2400, 656));
        plane.moved_to((3600, 1800), (400, 300));
        // The panel the app opens 200 pixels left of the button opens 200 left of it.
        assert_eq!(
            plane.place((3400, 1800), (1, 1), || unreachable!()),
            (200, 300)
        );
    }

    #[test]
    fn fr41_the_place_an_app_drag_left_is_remembered_per_app() {
        let dir = std::env::temp_dir().join(format!(
            "dsa-overlay-places-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        assert_eq!(remembered(&dir, "skplanet.musicmate"), None);

        let mut plane = OverlayPlane::new(None);
        plane.place((158, 82), (1, 1), || (106, 29));
        let anchor = plane.moved_to((158, 82), (3000, 900));
        remember(&dir, "skplanet.musicmate", anchor).unwrap();

        // The app shows its overlay again, at the same place on Android's display.
        let mut again = OverlayPlane::new(remembered(&dir, "skplanet.musicmate"));
        assert_eq!(
            again.place((158, 82), (1, 1), || unreachable!()),
            (3000, 900)
        );
        // Another app's overlays are placed afresh.
        assert_eq!(remembered(&dir, "com.example.chat"), None);
        // A name that is no package names no file.
        assert!(remember(&dir, "..\\elsewhere", anchor).is_err());
        assert_eq!(remembered(&dir, "..\\elsewhere"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr41_an_overlay_is_never_larger_than_its_monitors_work_area() {
        // What fits keeps its size.
        assert_eq!(shown_size((318, 86), WORK), (318, 86));
        // A display-wide bar on a narrower monitor is shown smaller, in proportion.
        assert_eq!(shown_size((3840, 200), WORK), (2560, 133));
        // A display-high panel too.
        assert_eq!(shown_size((300, 3840), WORK), (109, 1400));
    }

    #[test]
    fn fr41_a_window_off_every_monitor_is_brought_back() {
        assert_eq!(kept_on((900, 600), (318, 86), WORK), (900, 600));
        assert_eq!(kept_on((5000, -300), (318, 86), WORK), (2560 - 318, 0));
        // One larger than the work area keeps its top left corner in reach.
        assert_eq!(kept_on((-900, 50), (4000, 86), WORK), (0, 50));
    }

    /// Floating lyrics 508 by 138 at (2800, 82) of Android's display, shown at half size in a
    /// window at (1500, 600), pressed 100 pixels into it.
    fn lyrics_pressed() -> OverlayDrag {
        OverlayDrag::press(
            (1600, 620),
            (1500, 600),
            (1, 2),
            (2800, 82),
            (508, 138),
            DISPLAY,
        )
    }

    #[test]
    fn fr41_an_app_drag_carries_its_overlay_past_androids_display_onto_any_monitor() {
        let mut drag = lyrics_pressed();
        drag.pointer_moved((1650, 620), (1500, 600), (1, 2), 8);
        assert!(!drag.carried());
        // The app drags the lyrics 100 of its pixels right, the way the pointer went: the
        // window follows the pointer from here, where the plane has it now.
        drag.app_moved((2900, 82), (1550, 600), (1, 2));
        assert!(drag.carried());
        assert_eq!(drag.window_at((1550, 600), (1, 2)), (1550, 600));
        // Android stops the lyrics at its display's right edge (3332), where the plane has the
        // window at 1766. The pointer goes on onto the second monitor, and the window with it,
        // the pressed point still under the pointer. The app drags it down too.
        drag.pointer_moved((3200, 640), (1550, 600), (1, 2), 8);
        drag.app_moved((3332, 122), (1766, 620), (1, 2));
        assert_eq!(drag.window_at((1766, 620), (1, 2)), (3100, 620));
        // On a monitor that shows it at full size the pressed point stays under the pointer.
        assert_eq!(drag.window_at((1766, 620), (1, 1)), (3000, 600));
    }

    #[test]
    fn fr41_a_press_its_app_does_not_move_the_overlay_with_moves_nothing() {
        // A press that the app answers without moving the overlay (a button, a seek bar).
        let mut drag = lyrics_pressed();
        drag.pointer_moved((1800, 700), (1500, 600), (1, 2), 8);
        drag.app_moved((2800, 82), (1500, 600), (1, 2));
        assert!(!drag.carried());
        assert_eq!(drag.window_at((1500, 600), (1, 2)), (1500, 600));
        // An app that moves its overlay against the pointer is not dragging it with the
        // pointer: the window goes where the plane puts it.
        drag.app_moved((2700, 82), (1450, 600), (1, 2));
        assert!(!drag.carried());
        assert_eq!(drag.window_at((1450, 600), (1, 2)), (1450, 600));
        // A bar as wide as the display lies against both edges: pushing it moves nothing.
        let mut bar =
            OverlayDrag::press((600, 620), (0, 600), (1, 2), (0, 82), (3840, 138), DISPLAY);
        bar.pointer_moved((2000, 620), (0, 600), (1, 2), 8);
        assert!(!bar.carried());
    }

    #[test]
    fn fr41_an_app_drag_goes_on_past_the_edge_android_keeps_the_overlay_at() {
        // The lyrics lie against the display's right edge, where an earlier drag left them.
        let pressed = || {
            OverlayDrag::press(
                (1866, 620),
                (1766, 600),
                (1, 2),
                (3332, 82),
                (508, 138),
                DISPLAY,
            )
        };
        let mut drag = pressed();
        // A slip within the slop moves nothing.
        drag.pointer_moved((1870, 620), (1766, 600), (1, 2), 8);
        assert!(!drag.carried());
        // Pushed outward past it, the window follows the pointer, without a jump.
        drag.pointer_moved((1900, 620), (1766, 600), (1, 2), 8);
        assert_eq!(drag.window_at((1766, 600), (1, 2)), (1766, 600));
        drag.pointer_moved((2500, 620), (1766, 600), (1, 2), 8);
        assert_eq!(drag.window_at((1766, 600), (1, 2)), (2366, 600));
        // Inward the app moves it itself, so pushing inward takes nothing over.
        let mut inward = pressed();
        inward.pointer_moved((1800, 620), (1766, 600), (1, 2), 8);
        assert!(!inward.carried());
        // The top edge too.
        let mut top = OverlayDrag::press(
            (1600, 30),
            (1500, 20),
            (1, 2),
            (2800, 10),
            (508, 138),
            DISPLAY,
        );
        top.pointer_moved((1600, 5), (1500, 20), (1, 2), 8);
        assert_eq!(top.window_at((1500, 20), (1, 2)), (1500, 20));
        top.pointer_moved((1600, -20), (1500, 20), (1, 2), 8);
        assert_eq!(top.window_at((1500, 20), (1, 2)), (1500, -5));
    }

    #[test]
    fn fr41_the_app_hears_the_pointer_as_the_press_found_it() {
        let drag = lyrics_pressed();
        // Where the plane put the window when the press began, every point stays.
        assert_eq!(
            drag.input((1500, 600), (2800, 82), (1, 2)),
            InputRemap::SAME
        );
        // The window has followed the pointer to (3100, 620) while Android keeps the lyrics at
        // (3332, 122). The pointer at (3200, 640) is 100 by 20 into the window, and the app
        // hears it at 2800 + (3200 - 1500) * 2 = 6200 and 82 + (640 - 600) * 2 = 162, as it
        // would have without the window moving: 2868 by 40 past where the lyrics are.
        let remap = drag.input((3100, 620), (3332, 122), (1, 2));
        assert_eq!(remap.apply((100.0, 20.0)), (1434.0, 20.0));
        // On a monitor that shows the lyrics at full size, the same point of the desktop.
        let remap = drag.input((3000, 600), (3332, 122), (1, 1));
        assert_eq!(remap.apply((200.0, 40.0)), (2868.0, 40.0));
    }

    #[test]
    fn fr41_only_a_focusable_overlay_takes_the_focus_and_only_by_a_click() {
        assert!(!click_activates(false));
        assert!(click_activates(true));
    }
}
