//! How a task window's size and scale follow the user, and how its task follows the window.
//!
//! One guest pixel is one pixel of the window. A task's frames are shown 1:1, so they are as
//! sharp on a monitor at 200% as at 100%. What keeps an app the same size to the eye across
//! monitors is its density: the task draws at [`density_for_dpi`] of its window's monitor,
//! so a task that moves from a 100% to a 200% monitor draws at 262 dpi into about twice the
//! pixels, and its text stays sharp instead of being scaled up.
//!
//! The guest has one display, but density is set per task, so two windows on monitors of
//! different scales each get the density of their own monitor.
//!
//! That is the app way ([`ScalingMode::App`]). Android relaunches an activity whose density
//! changes unless the app says it handles that itself, and no app may be restarted for a
//! move, so a task whose app does not say so keeps its density, and its window shows its
//! frames scaled to the new monitor instead ([`ScalingMode::System`]): a task at 130 dpi on a
//! 200% monitor is shown at 262 window pixels per 130 task pixels, the size a task drawn there
//! at 262 dpi would be. The window's client area is then in window pixels and the task's size in task pixels, and
//! [`WindowSizing::scale`] turns one into the other. A window opens at 1:1, and takes a scale
//! from the first monitor scale it reports.
//!
//! [`WindowSizing`] holds one window's side of this. The platform tells it what the user and
//! the window manager did (a new client size, a new monitor scale, a drag starting or
//! ending) and carries out what it returns: a request to the guest, or a size the window
//! must take. It names no window system, so a window process of any platform can use it.

use std::time::{Duration, Instant};

use super::task_windows::{fit_within, window_size};
pub use crate::app_windows::ScalingMode;
use crate::tasks::{Bounds, Task, TaskRequest};

/// How large a window shows each of its task's pixels: `.0` window pixels per `.1` task
/// pixels, in lowest terms. `(1, 1)` is 1:1.
pub type Scale = (u32, u32);

/// The scale that shows a task drawn at `task_density` the size a task drawn at
/// `window_density` would be: `window_density` over `task_density`, in lowest terms. 1:1 when
/// either is unknown (0).
pub fn scale_between(window_density: u32, task_density: u32) -> Scale {
    if window_density == 0 || task_density == 0 || window_density == task_density {
        return (1, 1);
    }
    let (mut a, mut b) = (window_density, task_density);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    (window_density / a, task_density / a)
}

/// `length` task pixels in window pixels at `scale`, rounded to the nearest.
pub fn to_window_length(length: u32, (numerator, denominator): Scale) -> u32 {
    if numerator == denominator || denominator == 0 {
        return length;
    }
    ((u64::from(length) * u64::from(numerator) + u64::from(denominator) / 2)
        / u64::from(denominator))
    .min(u64::from(u32::MAX)) as u32
}

/// `length` window pixels in task pixels at `scale`, rounded to the nearest.
pub fn to_task_length(length: u32, (numerator, denominator): Scale) -> u32 {
    to_window_length(length, (denominator, numerator))
}

/// `size` task pixels in window pixels at `scale`, never smaller than one pixel.
pub fn to_window_size((width, height): (u32, u32), scale: Scale) -> (u32, u32) {
    (
        to_window_length(width, scale).max(1),
        to_window_length(height, scale).max(1),
    )
}

/// `size` window pixels in task pixels at `scale`, never smaller than one pixel.
pub fn to_task_size((width, height): (u32, u32), scale: Scale) -> (u32, u32) {
    (
        to_task_length(width, scale).max(1),
        to_task_length(height, scale).max(1),
    )
}

/// The client size a frame (or task) of `size` task pixels fills at `scale`, given the client
/// sizes the window has or asked for (`views`). A window size goes to the guest rounded to
/// whole task pixels, and that task size rounded back can land a pixel off the window size it
/// came from (at 131:65 a 1009 pixel window asks for 501, and 501 is 1010 again). So a frame
/// of the size a view was turned into is that view, not the size rounded back: otherwise the
/// window would take a size a pixel off the one the user dragged to, and the frame it asked
/// for would never count as its size. Any other frame is its size at `scale`.
pub fn window_size_of(size: (u32, u32), scale: Scale, views: &[(u32, u32)]) -> (u32, u32) {
    views
        .iter()
        .copied()
        .find(|view| view.0 != 0 && view.1 != 0 && to_task_size(*view, scale) == size)
        .unwrap_or_else(|| to_window_size(size, scale))
}

/// A rectangle in task pixels in window pixels at `scale`.
pub fn to_window_rect(rect: Bounds, (numerator, denominator): Scale) -> Bounds {
    let at = |v: i32| -> i32 {
        if numerator == denominator || denominator == 0 {
            return v;
        }
        let scaled = (i64::from(v) * i64::from(numerator)).div_euclid(i64::from(denominator));
        scaled.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    };
    Bounds {
        left: at(rect.left),
        top: at(rect.top),
        right: at(rect.right),
        bottom: at(rect.bottom),
    }
}

/// A position `position` task pixels from the task's top left, in window pixels from the
/// client area's top left at `scale`, unrounded: where the window shows that task pixel. A
/// scale with a 0 in it is 1:1.
pub fn to_window_position(position: f64, (numerator, denominator): Scale) -> f64 {
    if numerator == denominator || numerator == 0 || denominator == 0 {
        position
    } else {
        position * f64::from(numerator) / f64::from(denominator)
    }
}

/// A position `position` window pixels from the client area's top left, in task pixels from
/// the task's top left at `scale`, unrounded: the task pixel the window shows there. The
/// inverse of [`to_window_position`].
pub fn to_task_position(position: f64, (numerator, denominator): Scale) -> f64 {
    to_window_position(position, (denominator, numerator))
}

/// A monitor's dots per inch at a scale of 100%: a host device-independent pixel (DIP) is
/// 1/96 inch.
pub const BASE_DPI: u32 = 96;

/// The density at which an app draws one pixel per density-independent pixel (Android's
/// `DENSITY_DEFAULT`): a length in dp is `dp x density / 160` pixels.
pub const BASE_DENSITY: u32 = 160;

/// Android's density per unit of monitor scale, before rounding: 131 dpi at 100%.
///
/// Android sizes a desktop monitor's touch target at 48 dp for 10.4 mm, so its desktop dp is
/// 10.4 / 48 mm, or 1/117.23 inch (the default density of Android 16's external displays). A
/// host DIP is 1/96 inch at a scale of 100% and 1/96 inch times the scale `s` at `s`. A dp is
/// then `density / 160` pixels of a monitor of `96 x s` pixels per inch when
/// `density = 96 x s x (10.4 / 25.4) x (160 / 48) = 131.0 x s`, and 1 dp = 0.819 DIP. An app's
/// buttons, icons and margins come out at the size of a host app's: the phone's dp (1/160
/// inch) makes them too small, and one dp per DIP (WSA) leaves them at their touch size.
/// Text follows the same ratio, so the guest's font scale is the host's text size alone
/// ([`crate::guest_display::font_scale`]).
pub const DENSITY_PER_SCALE: u32 = 131;

/// Android's density on a monitor at a scale of 100%: [`density_for_dpi`] of [`BASE_DPI`].
pub const DENSITY_AT_BASE_DPI: u32 = 130;

/// How long the guest has to take a size the host asked for before the window follows the
/// size the guest did take instead. The agent reports the size it is giving a task as soon
/// as it asks for it, so the answer normally arrives in a few milliseconds; this is for a
/// guest that cannot give the size at all.
pub const SETTLE: Duration = Duration::from_secs(2);

/// The density a task draws at on a monitor of `dpi`, so it looks the size it would at 100%:
/// [`DENSITY_PER_SCALE`] times the monitor's scale, rounded to the nearest dpi and then down
/// to an even number: 130 at 96 dpi (100%), 164 at 120
/// (125%), 196 at 144 (150%), 228 at 168 (175%), 262 at 192 (200%), 328 at 240 (250%). A
/// monitor that reports no DPI counts as 100%.
pub fn density_for_dpi(dpi: u32) -> u32 {
    let dpi = if dpi == 0 { BASE_DPI } else { dpi };
    let density = (u64::from(dpi) * u64::from(DENSITY_PER_SCALE) + u64::from(BASE_DPI) / 2)
        / u64::from(BASE_DPI);
    (density & !1).min(u64::from(u32::MAX - 1)) as u32
}

/// `dp` density-independent pixels of an app drawn at `density`, in pixels, rounded to the
/// nearest. A density of 0 counts as the density at 100%.
pub fn dp_to_px(dp: u32, density: u32) -> u32 {
    let density = if density == 0 {
        DENSITY_AT_BASE_DPI
    } else {
        density
    };
    ((u64::from(dp) * u64::from(density) + u64::from(BASE_DENSITY) / 2) / u64::from(BASE_DENSITY))
        .min(u64::from(u32::MAX)) as u32
}

/// `px` pixels of an app drawn at `density`, in its density-independent pixels, rounded to
/// the nearest. A density of 0 counts as the density at 100%.
pub fn px_to_dp(px: u32, density: u32) -> u32 {
    let density = if density == 0 {
        DENSITY_AT_BASE_DPI
    } else {
        density
    };
    ((u64::from(px) * u64::from(BASE_DENSITY) + u64::from(density) / 2) / u64::from(density))
        .min(u64::from(u32::MAX)) as u32
}

/// `dip` host device-independent pixels on a monitor of `dpi`, in its pixels, rounded to the
/// nearest. A monitor that reports no DPI counts as 100%.
pub fn dip_to_px(dip: u32, dpi: u32) -> u32 {
    let dpi = if dpi == 0 { BASE_DPI } else { dpi };
    ((u64::from(dip) * u64::from(dpi) + u64::from(BASE_DPI) / 2) / u64::from(BASE_DPI))
        .min(u64::from(u32::MAX)) as u32
}

/// `size`, drawn at density `from`, rescaled to density `to`: the same size in
/// density-independent pixels. Unchanged when either density is unknown (0).
pub fn rescale(size: (u32, u32), from: u32, to: u32) -> (u32, u32) {
    if from == 0 || to == 0 || from == to {
        return size;
    }
    let scale = |length: u32| {
        let scaled = (u64::from(length) * u64::from(to) + u64::from(from) / 2) / u64::from(from);
        scaled.clamp(1, u64::from(u32::MAX)) as u32
    };
    (scale(size.0), scale(size.1))
}

/// `size` cut down to `max` in each direction, when there is a limit.
pub fn clamp_size(size: (u32, u32), max: Option<(u32, u32)>) -> (u32, u32) {
    match max {
        Some((width, height)) => (size.0.min(width.max(1)), size.1.min(height.max(1))),
        None => size,
    }
}

/// Where a maximised window of `outer` pixels goes, along one direction, on a work area
/// that starts at `start` and is `area` pixels long, when the window's border on that side
/// is `border` pixels thick. A window is never larger than the guest can draw, so on a
/// monitor larger than that it maximises to the guest's largest size, shown 1:1, and is
/// centred on the work area rather than left in its corner. A window that fills the work
/// area hangs its border off the work area's edge, so its client area starts exactly at
/// `start`. That holds wherever the window was before: a maximised window that takes a
/// frame of the work area's size while it still stands where its smaller frame was centred
/// goes to the work area's edge, rather than staying put and running off the screen.
pub fn maximized_position(start: i32, area: i32, outer: i32, border: i32) -> i32 {
    if outer >= area {
        return start - border;
    }
    start + (area - outer) / 2
}

/// The outer size a maximised task window takes: the most the guest can draw (`guest`, with
/// the borders of a maximised window) and no more than the work area of the window's own
/// monitor with those borders, which Windows hangs off its edges (`work`). Windows proposes a
/// maximised size measured on the primary monitor, and adjusts it to the monitor the window
/// maximises on only while it is at least the primary monitor's size; a smaller one, such as
/// the guest's largest size, it takes as it is. So on a monitor smaller than the primary one
/// a window would maximise past the bottom of its own screen unless its work area limits it.
pub fn maximized_outer(guest: (i32, i32), work: (i32, i32)) -> (i32, i32) {
    (guest.0.min(work.0).max(1), guest.1.min(work.1).max(1))
}

/// Where a window goes and how large it is (left, top, width, height, all of its outer
/// rectangle) when its monitor changes scale and the desktop suggests `suggested` for it.
/// That is the suggested rectangle itself, cut down to `limit`, the largest outer size the
/// window may have there: what the guest can draw, and no more than the work area of the
/// monitor it moves to. A rectangle that is cut down stays centred on the suggested one, so
/// it is still on the monitor the desktop chose, and the desktop does not move the window's
/// scale back.
pub fn rescaled_rect(suggested: (i32, i32, i32, i32), limit: (i32, i32)) -> (i32, i32, i32, i32) {
    let (left, top, width, height) = suggested;
    let (cut_width, cut_height) = (width.min(limit.0.max(1)), height.min(limit.1.max(1)));
    (
        left + (width - cut_width) / 2,
        top + (height - cut_height) / 2,
        cut_width,
        cut_height,
    )
}

/// The smallest a task window's client area gets, in density-independent pixels in each
/// direction: the smallest task Android's own freeform windows let a user make. Below it the
/// guest would keep the task larger than the window, so no frame could ever show 1:1, and
/// apps are not designed to be laid out in less.
pub const MIN_CLIENT_DP: u32 = 220;

/// The smallest client area a window on a monitor of `dpi` may have, in pixels: at least
/// `task`, the smallest the guest gives the task (an app may declare more than the default),
/// and within `max`, the most the guest can draw, when that is smaller.
pub fn min_client_size(dpi: u32, max: Option<(u32, u32)>, task: Option<(u32, u32)>) -> (u32, u32) {
    // Android's own minimum is in its dp, at the density the task draws at there.
    let side = dp_to_px(MIN_CLIENT_DP, density_for_dpi(dpi));
    let (task_width, task_height) = task.unwrap_or((0, 0));
    let (width, height) = (side.max(task_width), side.max(task_height));
    match max {
        Some((max_width, max_height)) => {
            (width.min(max_width.max(1)), height.min(max_height.max(1)))
        }
        None => (width, height),
    }
}

/// A task's size in the guest, and the largest and the smallest it can have.
pub type TaskSize = ((u32, u32), Option<(u32, u32)>, Option<(u32, u32)>);

/// One task window's size and density, as the host and the guest each see them.
#[derive(Debug, Clone)]
pub struct WindowSizing {
    task_id: i32,
    /// The task's size as the guest last reported it.
    guest: (u32, u32),
    /// The density the guest last reported for the task; 0 when unknown.
    density: u32,
    /// The largest size the guest can give the task.
    max: Option<(u32, u32)>,
    /// The smallest size the guest gives the task.
    min: Option<(u32, u32)>,
    /// The window's client size.
    view: (u32, u32),
    /// The size last asked of the guest that it has not yet reported, and when.
    asked: Option<((u32, u32), Instant)>,
    /// The density last asked of the guest.
    asked_density: Option<u32>,
    /// Whether the user is dragging the window's edge or moving it.
    dragging: bool,
    /// Whether the task was given a size live during this drag, so it must be reconfigured
    /// when the drag ends.
    resized_live: bool,
    /// Whether the task was given a size live outside a drag (a maximise, a snap) and still
    /// has to be reconfigured for it once it has taken it.
    reconfigure: bool,
    /// Whether the window has said which monitor scale it opened on.
    placed: bool,
    /// Whether the task follows its window's monitor's density or keeps its own.
    mode: ScalingMode,
    /// The density of the window's monitor, once the window has said; 0 until then.
    monitor_density: u32,
    /// The density the task keeps while its window is scaled ([`ScalingMode::System`]); 0
    /// while it follows its monitor. Fixed while the window is scaled, so a report of the task
    /// in between cannot change the scale under the window.
    kept_density: u32,
}

impl WindowSizing {
    /// A window that shows `task` at the size the guest gave it, whose task follows its
    /// monitor's density.
    pub fn new(task: &Task) -> WindowSizing {
        WindowSizing::with_mode(task, ScalingMode::App)
    }

    /// A window that shows `task` at the size the guest gave it, following its monitor's
    /// scale in `mode`.
    pub fn with_mode(task: &Task, mode: ScalingMode) -> WindowSizing {
        let guest = window_size(&task.bounds);
        WindowSizing {
            task_id: task.task_id,
            guest,
            density: task.density_dpi,
            max: task.max_size(),
            min: task.min_size(),
            view: guest,
            asked: None,
            asked_density: None,
            dragging: false,
            resized_live: false,
            reconfigure: false,
            placed: false,
            mode,
            monitor_density: 0,
            kept_density: match mode {
                ScalingMode::App => 0,
                ScalingMode::System => task.density_dpi,
            },
        }
    }

    /// How the task follows its window's monitor now.
    pub fn mode(&self) -> ScalingMode {
        self.mode
    }

    /// The task's app now says it does (`App`) or does not handle a change of density, as the
    /// user's choice for it reads. A window being scaled goes on being scaled until its scale is
    /// 1:1 again, since giving the task its monitor's density then would change it. A task that
    /// stops following its monitor keeps the density it has.
    pub fn set_mode(&mut self, mode: ScalingMode) {
        match mode {
            ScalingMode::System if self.mode == ScalingMode::App => {
                self.mode = ScalingMode::System;
                self.kept_density = self.asked_density.unwrap_or(self.density);
            }
            ScalingMode::App if self.mode == ScalingMode::System && self.scale() == (1, 1) => {
                self.mode = ScalingMode::App;
                self.kept_density = 0;
            }
            _ => {}
        }
    }

    /// How large the window shows each of the task's pixels: 1:1 while the task follows its
    /// monitor's density, else the monitor's density over the density the task keeps.
    pub fn scale(&self) -> Scale {
        match self.mode {
            ScalingMode::App => (1, 1),
            ScalingMode::System => scale_between(self.monitor_density, self.kept_density),
        }
    }

    /// The largest client area the window may have: what the guest can draw, at the window's
    /// scale. A larger window could never match a frame of the task.
    pub fn max_client_size(&self) -> Option<(u32, u32)> {
        self.max.map(|max| to_window_size(max, self.scale()))
    }

    /// The task's size as the guest last reported it.
    pub fn guest_size(&self) -> (u32, u32) {
        self.guest
    }

    /// Whether the window is waiting for the guest to take a size it asked for.
    pub fn waiting(&self) -> bool {
        self.asked.is_some()
    }

    /// The window opened on a monitor of `dpi`. Returns the client size it opens at (the
    /// task's size rescaled to the monitor's density, within what the guest can draw) and
    /// the density request, if the task draws at another density. The window then reports
    /// its client size through [`view_resized`](Self::view_resized), which asks the guest
    /// for that size.
    ///
    /// A task that keeps its density opens at the same size to the eye: its window shows it
    /// scaled, and no density is asked for.
    pub fn opened(&mut self, dpi: u32) -> ((u32, u32), Option<TaskRequest>) {
        let request = self.dpi_changed(dpi);
        let size = rescale(self.guest, self.density, density_for_dpi(dpi));
        let size = match self.max_client_size() {
            Some(max) => fit_within(size, max),
            None => size,
        };
        (size, request)
    }

    /// The window reports the scale of its monitor. The first report is the monitor it
    /// opened on ([`opened`](Self::opened)): it returns the client size the window takes,
    /// when that differs from the size it has, and the density request. Every later report
    /// is a move to another monitor ([`dpi_changed`](Self::dpi_changed)).
    pub fn dpi_reported(&mut self, dpi: u32) -> (Option<(u32, u32)>, Option<TaskRequest>) {
        if self.placed {
            return (None, self.dpi_changed(dpi));
        }
        self.placed = true;
        let (size, request) = self.opened(dpi);
        if size == self.view {
            return (None, request);
        }
        self.view = size;
        (Some(size), request)
    }

    /// The task's size as the guest has it, which the window measures its pointer input in,
    /// and the largest and smallest client areas the window keeps to: the sizes the guest
    /// gives the task, at the window's scale.
    pub fn task_size(&self) -> TaskSize {
        let scale = self.scale();
        (
            self.guest,
            self.max.map(|max| to_window_size(max, scale)),
            self.min.map(|min| to_window_size(min, scale)),
        )
    }

    /// The window is now on a monitor of `dpi`. Returns the request that has the task draw
    /// at that monitor's density, unless it does already. The window manager resizes the
    /// window to keep its size to the eye, and that new size follows through
    /// [`view_resized`](Self::view_resized), so the guest gets the density first and the
    /// size right after it.
    ///
    /// A task that keeps its density ([`ScalingMode::System`]) is asked for nothing: its
    /// window takes the scale between the monitor's density and the task's, and the new size
    /// the window manager gives the window is the task's own size at that scale.
    pub fn dpi_changed(&mut self, dpi: u32) -> Option<TaskRequest> {
        let wanted = density_for_dpi(dpi);
        self.monitor_density = wanted;
        if self.mode == ScalingMode::System {
            return None;
        }
        if self.asked_density.unwrap_or(self.density) == wanted {
            return None;
        }
        self.asked_density = Some(wanted);
        Some(TaskRequest::SetDensity {
            task_id: self.task_id,
            density_dpi: wanted,
        })
    }

    /// The window's client area is now `view`. Returns the request that gives the task that
    /// size, unless the task has it or it was asked for already. Called for every step of a
    /// live resize; the requests are folded together before they are sent.
    ///
    /// The request is always live: the task takes the size without being reconfigured, so an
    /// app that does not handle a change of screen size is laid out again within a frame
    /// rather than recreated at the new size. Recreating an app can take longer than the
    /// guest holds a resized task's frames, and a maximise asked for at once then showed the
    /// old, small picture on black before the app had drawn again.
    ///
    /// The task is reconfigured afterwards, at a size it already has: at the end of the drag
    /// ([`dragging`](Self::dragging)), or, for a change of size outside a drag (a maximise,
    /// a snap, a move to a monitor of another scale), once the guest has taken the size
    /// ([`reconfigure`](Self::reconfigure)).
    pub fn view_resized(&mut self, view: (u32, u32), now: Instant) -> Option<TaskRequest> {
        if view.0 == 0 || view.1 == 0 {
            return None;
        }
        self.view = view;
        let size = clamp_size(to_task_size(view, self.scale()), self.max);
        let current = self.asked.map(|(size, _)| size).unwrap_or(self.guest);
        if size == current {
            return None;
        }
        self.asked = Some((size, now));
        if self.dragging {
            self.resized_live = true;
        } else {
            self.reconfigure = true;
        }
        Some(TaskRequest::Resize {
            task_id: self.task_id,
            width: size.0,
            height: size.1,
            live: true,
        })
    }

    /// Returns the request (not live) that reconfigures the task for a size it was given
    /// live outside a drag, once the guest has taken that size, or once it has had
    /// [`SETTLE`] to and the window has gone back to the size the task has
    /// ([`tick`](Self::tick)). The task has that size already, so it is recreated in place
    /// and the window waits for nothing. `None` while there is nothing to reconfigure, while
    /// the guest is still on its way, and while the user drags the window, whose end
    /// reconfigures the task instead.
    pub fn reconfigure(&mut self, now: Instant) -> Option<TaskRequest> {
        if !self.reconfigure || self.dragging {
            return None;
        }
        if let Some((_, since)) = self.asked {
            if now.saturating_duration_since(since) < SETTLE {
                return None;
            }
        }
        self.reconfigure = false;
        Some(TaskRequest::Resize {
            task_id: self.task_id,
            width: self.guest.0,
            height: self.guest.1,
            live: false,
        })
    }

    /// The user started (`true`) or finished dragging the window. While they drag, the
    /// window never resizes itself under them. The guest has [`SETTLE`] from the end of the
    /// drag to take the last size.
    ///
    /// When a drag that resized the task live ends, returns the request (not live) that
    /// gives the task the window's size and reconfigures it for that size. It is sent even
    /// when the task has that size already: the live steps left the task with the
    /// configuration it had when the drag began. A drag that only moved the window asks
    /// nothing.
    pub fn dragging(&mut self, active: bool, now: Instant) -> Option<TaskRequest> {
        if active {
            if !self.dragging {
                self.dragging = true;
                // A size given live just before the drag is reconfigured with the drag's.
                self.resized_live = std::mem::take(&mut self.reconfigure);
            }
            return None;
        }
        if !self.dragging {
            return None;
        }
        self.dragging = false;
        if !std::mem::take(&mut self.resized_live) {
            if let Some((size, _)) = self.asked {
                self.asked = Some((size, now));
            }
            return None;
        }
        let size = clamp_size(to_task_size(self.view, self.scale()), self.max);
        // A task that has the size already reports the same bounds after it is
        // reconfigured, if it reports at all, so there is nothing to wait for.
        self.asked = (size != self.guest).then_some((size, now));
        Some(TaskRequest::Resize {
            task_id: self.task_id,
            width: size.0,
            height: size.1,
            live: false,
        })
    }

    /// The guest reported the task. Returns the client size the window must take, when the
    /// guest resized the task on its own; `None` when the window keeps its size, as it does
    /// while the guest catches up with sizes the host asked for.
    pub fn guest_updated(&mut self, task: &Task) -> Option<(u32, u32)> {
        let size = window_size(&task.bounds);
        self.density = task.density_dpi;
        if self.mode == ScalingMode::System && self.kept_density == 0 {
            // The agent could not read the density when the window opened.
            self.kept_density = task.density_dpi;
        }
        if let Some(max) = task.max_size() {
            self.max = Some(max);
        }
        if let Some(min) = task.min_size() {
            self.min = Some(min);
        }
        let previous = std::mem::replace(&mut self.guest, size);
        let shown = window_size_of(size, self.scale(), &[self.view]);
        match self.asked {
            Some((asked, _)) if asked == size => {
                self.asked = None;
                None
            }
            // Still on its way to the size last asked for.
            Some(_) => None,
            None if size != previous && shown != self.view && !self.dragging => {
                self.view = shown;
                Some(shown)
            }
            None => None,
        }
    }

    /// Time has passed. Returns the client size the window must take when the guest has not
    /// taken the size the host asked for within [`SETTLE`]: the window then shows the task
    /// at the size it has, 1:1, rather than wait for a size that never comes.
    pub fn tick(&mut self, now: Instant) -> Option<(u32, u32)> {
        let (_, since) = self.asked?;
        if self.dragging || now.saturating_duration_since(since) < SETTLE {
            return None;
        }
        self.asked = None;
        let shown = window_size_of(self.guest, self.scale(), &[self.view]);
        if shown == self.view {
            return None;
        }
        self.view = shown;
        Some(shown)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::Bounds;

    fn task(width: i32, height: i32, density_dpi: u32) -> Task {
        Task {
            task_id: 7,
            bounds: Bounds {
                left: 30,
                top: 40,
                right: 30 + width,
                bottom: 40 + height,
            },
            package_name: "com.example.notes".into(),
            density_dpi,
            max_width: 1920,
            max_height: 1080,
            ..Task::default()
        }
    }

    fn resize(width: u32, height: u32) -> Option<TaskRequest> {
        Some(TaskRequest::Resize {
            task_id: 7,
            width,
            height,
            live: false,
        })
    }

    fn live_resize(width: u32, height: u32) -> Option<TaskRequest> {
        Some(TaskRequest::Resize {
            task_id: 7,
            width,
            height,
            live: true,
        })
    }

    fn density(density_dpi: u32) -> Option<TaskRequest> {
        Some(TaskRequest::SetDensity {
            task_id: 7,
            density_dpi,
        })
    }

    #[test]
    fn fr4_sizes_asked_while_dragging_are_live_and_the_drag_end_reconfigures_once() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        let now = Instant::now();
        assert_eq!(sizing.dragging(true, now), None);
        assert_eq!(sizing.view_resized((500, 300), now), live_resize(500, 300));
        assert_eq!(sizing.view_resized((600, 300), now), live_resize(600, 300));
        // The guest takes each live size and reports it as it would any other.
        assert_eq!(sizing.guest_updated(&task(500, 300, 130)), None);
        assert_eq!(sizing.guest_updated(&task(600, 300, 130)), None);
        assert!(!sizing.waiting());
        // The task has the size already; the end of the drag asks for it again, not live,
        // which is what reconfigures it, and waits for nothing.
        assert_eq!(sizing.dragging(false, now), resize(600, 300));
        assert!(!sizing.waiting());
        // The guest's report of it carries the same bounds and changes nothing.
        assert_eq!(sizing.guest_updated(&task(600, 300, 130)), None);
        assert_eq!(sizing.tick(now + SETTLE * 2), None);
        // A change of size outside a drag (a maximise, a snap) is live too.
        assert_eq!(sizing.view_resized((900, 700), now), live_resize(900, 700));
    }

    #[test]
    fn fr4_a_maximise_gives_the_task_its_size_live_then_reconfigures_it_at_that_size() {
        // A 608x1080 task on a monitor at 200%; the guest draws at most 1920x1080.
        let mut sizing = WindowSizing::new(&task(608, 1080, 320));
        let now = Instant::now();
        assert_eq!(sizing.reconfigure(now), None, "nothing to reconfigure");
        // The window is maximised to the most the guest can draw: no drag, one size.
        assert_eq!(
            sizing.view_resized((1920, 1080), now),
            live_resize(1920, 1080)
        );
        // Not before the guest has taken the size: the app would be recreated at a size it
        // has not drawn at, and the window would show the old picture on black meanwhile.
        assert_eq!(sizing.reconfigure(now), None);
        assert_eq!(sizing.guest_updated(&task(1920, 1080, 320)), None);
        // Then once, at the size the task has, which the window keeps.
        assert_eq!(sizing.reconfigure(now), resize(1920, 1080));
        assert_eq!(sizing.reconfigure(now), None, "asked once");
        assert_eq!(sizing.guest_updated(&task(1920, 1080, 320)), None);
        assert_eq!(sizing.tick(now + SETTLE * 2), None);
        // Restoring the window goes the same way.
        assert_eq!(
            sizing.view_resized((608, 1080), now),
            live_resize(608, 1080)
        );
        assert_eq!(sizing.guest_updated(&task(608, 1080, 320)), None);
        assert_eq!(sizing.reconfigure(now), resize(608, 1080));
    }

    #[test]
    fn fr4_a_maximise_on_a_monitor_larger_than_the_guest_asks_for_no_more_than_it_draws() {
        // A 4K monitor's work area at 200% is larger than the 1920x1080 the guest draws.
        let mut sizing = WindowSizing::new(&task(608, 1080, 320));
        assert_eq!(
            sizing.view_resized((3840, 2054), Instant::now()),
            live_resize(1920, 1080)
        );
        assert_eq!(sizing.max_client_size(), Some((1920, 1080)));
    }

    #[test]
    fn fr4_a_size_the_guest_never_takes_still_ends_with_the_task_reconfigured() {
        let mut sizing = WindowSizing::new(&task(608, 1080, 320));
        let start = Instant::now();
        sizing.view_resized((1920, 1080), start);
        assert_eq!(sizing.reconfigure(start + SETTLE / 2), None);
        // The window goes back to the task's size, and the task is reconfigured at it, so
        // it does not keep the configuration the live size held.
        assert_eq!(sizing.tick(start + SETTLE), Some((608, 1080)));
        assert_eq!(sizing.reconfigure(start + SETTLE), resize(608, 1080));
    }

    #[test]
    fn fr4_a_drag_that_starts_before_a_maximise_is_reconfigured_reconfigures_it_once() {
        let mut sizing = WindowSizing::new(&task(608, 1080, 320));
        let now = Instant::now();
        sizing.view_resized((1920, 1080), now);
        sizing.guest_updated(&task(1920, 1080, 320));
        // The user takes hold of the window before the reconfigure went out.
        assert_eq!(sizing.dragging(true, now), None);
        assert_eq!(sizing.reconfigure(now), None, "not under a drag");
        // A drag that only moves it still ends with the task reconfigured, once.
        assert_eq!(sizing.dragging(false, now), resize(1920, 1080));
        assert_eq!(sizing.reconfigure(now), None);
    }

    #[test]
    fn fr4_a_window_maximised_on_a_monitor_smaller_than_the_primary_stays_on_its_screen() {
        // The guest draws 2880x2880, 1803x1800 at the window's scale: 1819x1816 outer with a
        // maximised window's borders at 100%. On a 2048x1152 monitor whose work area is
        // 2048x1104, the window fills the work area (2064x1120 with the borders) in height.
        assert_eq!(maximized_outer((1819, 1816), (2064, 1120)), (1819, 1120));
        // On a 4K monitor the guest's size is the smaller, and the window is centred instead.
        assert_eq!(maximized_outer((1819, 1816), (3856, 2128)), (1819, 1816));
        // A guest larger than every monitor fills the work area.
        assert_eq!(maximized_outer((8016, 8016), (2064, 1120)), (2064, 1120));
    }

    #[test]
    fn fr4_a_window_maximised_smaller_than_the_work_area_is_centred_on_it() {
        // A 1920x1080 client (1936x1119 outer at 100%) on a 3840x2100 work area at (0, 0).
        assert_eq!(maximized_position(0, 3840, 1936, 8), 952);
        assert_eq!(maximized_position(0, 2100, 1119, 8), 490);
        // On a second monitor, from its own corner.
        assert_eq!(maximized_position(2560, 3840, 1936, 8), 2560 + 952);
        // A window as large as the work area, borders and all, hangs its border off the edge.
        assert_eq!(maximized_position(0, 1920, 1936, 8), -8);
        assert_eq!(maximized_position(0, 1920, 1920, 0), 0);
    }

    #[test]
    fn fr4_a_window_that_fills_the_work_area_starts_at_its_edge_wherever_it_was() {
        // A maximised window took the work area's frame (1040x736 outer at 100%, a 1024x720
        // client) while it still stood where its smaller frame was centred, at (304, 202).
        // Its client area starts at the work area's corner all the same.
        assert_eq!(maximized_position(0, 1024, 1040, 8), -8);
        assert_eq!(maximized_position(0, 720, 736, 8), -8);
        // On a second monitor to the right of the primary, and on one above it.
        assert_eq!(maximized_position(1920, 2048, 2064, 8), 1912);
        assert_eq!(maximized_position(-1152, 1104, 1120, 8), -1160);
    }

    #[test]
    fn fr4_a_window_rescaled_larger_than_its_monitor_stays_within_it_and_on_it() {
        // A 2880x2707 window on a 125% monitor moves to a 200% one of 2880x1800 whose work area
        // is 2880x1728; the desktop suggests it 1.6 times larger. The guest draws 2880x2880,
        // which with the window's borders is 2912x2896: the work area is the smaller in both.
        let suggested = (-1700, -400, 4643, 4348);
        let limit = (2880, 1728);
        let (left, top, width, height) = rescaled_rect(suggested, limit);
        assert_eq!((width, height), (2880, 1728), "within the work area");
        // Centred on the suggested rectangle, whose middle stays where it was.
        assert_eq!(left + width / 2, -1700 + 4643 / 2);
        assert_eq!(top + height / 2, -400 + 4348 / 2);
        // A suggestion that fits is taken as it is.
        assert_eq!(
            rescaled_rect((100, 50, 1670, 1068), limit),
            (100, 50, 1670, 1068)
        );
    }

    #[test]
    fn fr4_the_drag_end_asks_for_a_size_the_guest_has_not_reported_yet() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        let start = Instant::now();
        sizing.dragging(true, start);
        assert_eq!(
            sizing.view_resized((700, 300), start),
            live_resize(700, 300)
        );
        let end = start + SETTLE / 4;
        assert_eq!(sizing.dragging(false, end), resize(700, 300));
        assert!(sizing.waiting());
        // The answer to either request clears the wait.
        assert_eq!(sizing.guest_updated(&task(700, 300, 130)), None);
        assert!(!sizing.waiting());
        assert_eq!(sizing.tick(end + SETTLE * 2), None);
    }

    #[test]
    fn fr4_the_drag_end_asks_for_no_more_than_the_guest_can_draw() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        let now = Instant::now();
        sizing.dragging(true, now);
        assert_eq!(
            sizing.view_resized((2560, 900), now),
            live_resize(1920, 900)
        );
        assert_eq!(sizing.dragging(false, now), resize(1920, 900));
    }

    #[test]
    fn fr4_a_drag_that_does_not_resize_the_task_asks_nothing_at_its_end() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        let now = Instant::now();
        // The user only moves the window.
        assert_eq!(sizing.dragging(true, now), None);
        assert_eq!(sizing.dragging(false, now), None);
        // A size the task has already is not a resize either.
        sizing.dragging(true, now);
        assert_eq!(sizing.view_resized((400, 300), now), None);
        assert_eq!(sizing.dragging(false, now), None);
        // An end the window reports twice asks once.
        sizing.dragging(true, now);
        sizing.view_resized((500, 300), now);
        assert_eq!(sizing.dragging(false, now), resize(500, 300));
        assert_eq!(sizing.dragging(false, now), None);
    }

    #[test]
    fn fr4_a_start_the_window_reports_twice_does_not_forget_the_live_sizes() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        let now = Instant::now();
        sizing.dragging(true, now);
        sizing.view_resized((500, 300), now);
        assert_eq!(sizing.dragging(true, now), None);
        assert_eq!(sizing.dragging(false, now), resize(500, 300));
    }

    #[test]
    fn fr4_a_window_stays_at_least_the_smallest_task_size_on_its_monitor() {
        // 220 dp at 130, 196 and 262 dpi.
        assert_eq!(min_client_size(96, None, None), (179, 179));
        assert_eq!(min_client_size(144, None, None), (270, 270));
        assert_eq!(min_client_size(192, Some((1920, 1080)), None), (360, 360));
        assert_eq!(
            min_client_size(0, None, None),
            (179, 179),
            "no DPI counts as 100%"
        );
        // A guest that draws less than that decides.
        assert_eq!(min_client_size(192, Some((200, 1080)), None), (200, 360));
    }

    #[test]
    fn fr4_a_window_respects_a_minimum_its_app_declares_above_the_default() {
        assert_eq!(min_client_size(96, None, Some((700, 500))), (700, 500));
        // Each direction on its own: the default still holds where the app asks for less.
        assert_eq!(min_client_size(96, None, Some((700, 100))), (700, 179));
        assert_eq!(min_client_size(192, None, Some((200, 500))), (360, 500));
        // Never more than the guest can draw.
        assert_eq!(
            min_client_size(96, Some((600, 1080)), Some((700, 500))),
            (600, 500)
        );
    }

    #[test]
    fn fr4_the_window_hears_its_tasks_minimum_and_any_change_to_it() {
        let mut declared = task(400, 300, 130);
        declared.min_width = 700;
        declared.min_height = 500;
        let mut sizing = WindowSizing::new(&declared);
        assert_eq!(sizing.task_size().2, Some((700, 500)));
        // Another activity on top of the task declares another minimum.
        declared.min_width = 800;
        sizing.guest_updated(&declared);
        assert_eq!(sizing.task_size().2, Some((800, 500)));
    }

    #[test]
    fn fr4_a_scaled_frame_of_the_asked_size_fills_exactly_the_size_dragged_to() {
        use crate::compositor::frame_follow::{Arrival, FrameFollow};
        use crate::compositor::resize_latency::ResizeLatency;
        // A 200% monitor showing a task drawn at 130 dpi: 1009x700 window pixels go to the
        // guest as 501x347, and 501x347 rounded back is 1010x699.
        let scale = scale_between(262, 130);
        assert_eq!(to_task_size((1009, 700), scale), (501, 347));
        assert_eq!(to_window_size((501, 347), scale), (1010, 699));
        // The frame of the size asked for is the size asked for.
        assert_eq!(
            window_size_of((501, 347), scale, &[(1009, 700)]),
            (1009, 700)
        );
        // Any other frame is its own size at the scale, and an empty size names nothing.
        assert_eq!(
            window_size_of((500, 347), scale, &[(1009, 700)]),
            (1008, 699)
        );
        assert_eq!(window_size_of((1, 1), scale, &[(0, 0)]), (2, 2));
        // 1:1 is the frame's size, as before.
        assert_eq!(
            window_size_of((800, 600), (1, 1), &[(801, 600)]),
            (800, 600)
        );

        // The window shows a 1009x700 frame and the user drags to 1009x700 from 900x700:
        // the frame of 501x347 is shown at once, not kept for a window a pixel off the drag.
        let mut follow = FrameFollow::new((900, 700));
        let mut latency = ResizeLatency::new();
        let now = Instant::now();
        let first = window_size_of((447, 347), scale, &[follow.view()]);
        assert_eq!(follow.frame_arrived(first), Arrival::Show);
        latency.frame_shown(first, now);
        follow.view_taken((1009, 700));
        latency.view_changed((1009, 700), now);
        let frame = window_size_of((501, 347), scale, &[follow.view(), (1009, 700)]);
        assert_eq!(follow.frame_arrived(frame), Arrival::Show);
        // And the wait for it ends with it, so it is neither lost from the summary nor
        // reported as a window stuck at its old size.
        assert!(latency
            .frame_shown(frame, now + Duration::from_millis(80))
            .is_some());
        assert_eq!(latency.stuck(now + Duration::from_secs(3)), None);
    }

    #[test]
    fn fr4_a_task_that_keeps_its_density_is_shown_at_the_same_physical_size() {
        assert_eq!(scale_between(262, 130), (131, 65));
        assert_eq!(scale_between(196, 130), (98, 65));
        assert_eq!(scale_between(160, 240), (2, 3));
        assert_eq!(scale_between(0, 130), (1, 1));
        assert_eq!(to_window_size((400, 300), (3, 2)), (600, 450));
        assert_eq!(to_task_size((600, 450), (3, 2)), (400, 300));
        let mut sizing = WindowSizing::with_mode(&task(400, 300, 130), ScalingMode::System);
        // It opens on a 150% monitor: the same size to the eye as a task drawn there at 196
        // dpi, and no density asked for.
        assert_eq!(sizing.dpi_reported(144), (Some((603, 452)), None));
        assert_eq!(sizing.scale(), (98, 65));
        assert_eq!(sizing.max_client_size(), Some((2895, 1628)));
        assert_eq!(
            sizing.task_size(),
            ((400, 300), Some((2895, 1628)), None),
            "the task keeps its size; the limits are the window's"
        );
        let now = Instant::now();
        assert_eq!(sizing.view_resized((603, 452), now), None);
        // A move to a 200% monitor asks nothing either.
        assert_eq!(sizing.dpi_changed(192), None);
        assert_eq!(sizing.scale(), (131, 65));
        assert_eq!(sizing.view_resized((806, 605), now), None);
        // The guest resizing the task on its own resizes the window at the scale.
        assert_eq!(
            sizing.guest_updated(&task(500, 300, 130)),
            Some((1008, 605))
        );
    }

    #[test]
    fn fr4_density_follows_the_monitor_scale() {
        assert_eq!(density_for_dpi(96), 130);
        assert_eq!(density_for_dpi(120), 164);
        assert_eq!(density_for_dpi(144), 196);
        assert_eq!(density_for_dpi(168), 228);
        assert_eq!(density_for_dpi(192), 262);
        assert_eq!(density_for_dpi(240), 328);
        assert_eq!(density_for_dpi(288), 392);
        assert_eq!(density_for_dpi(96), DENSITY_AT_BASE_DPI);
        assert_eq!(
            density_for_dpi(0),
            130,
            "a monitor that reports nothing is 100%"
        );
        for dpi in 1..=960 {
            assert_eq!(density_for_dpi(dpi) % 2, 0, "{dpi} dpi: densities are even");
        }
    }

    #[test]
    fn fr4_dp_and_dip_convert_by_their_definitions_at_each_scale() {
        // Android's desktop dp is 10.4 / 48 mm (1/117.23 inch) and a DIP 1/96 inch at 100%, so
        // a dp is 0.819 DIP and Android draws at 131 x the scale, rounded to an even density.
        // (scale in percent, the monitor's DPI, Android's density there, 100 dp in pixels, the
        // smallest task Android allows, 220 dp, in pixels)
        let scales = [
            (100, 96, 130, 81, 179),
            (125, 120, 164, 103, 226),
            (150, 144, 196, 123, 270),
            (175, 168, 228, 143, 314),
            (200, 192, 262, 164, 360),
            (250, 240, 328, 205, 451),
        ];
        for (percent, dpi, density, px_100_dp, px_220_dp) in scales {
            assert_eq!(density_for_dpi(dpi), density, "{percent}%");
            assert_eq!(dp_to_px(100, density), px_100_dp, "{percent}%");
            assert_eq!(px_to_dp(px_100_dp, density), 100, "{percent}%");
            assert_eq!(min_client_size(dpi, None, None).0, px_220_dp, "{percent}%");
            // 100 dp is 81.9 DIP, which is 81.9 x the scale in the monitor's pixels. Rounding
            // the density to an even number moves it by less than 1%, and the pixel count
            // then rounds to a whole pixel, which at 100% is itself more than 1% of 81 pixels.
            let dip = 100.0 * 96.0 * (10.4 / 25.4) * (160.0 / 48.0) / 160.0;
            let wanted = dip * f64::from(percent) / 100.0;
            let exact_density = wanted * 160.0 / 100.0;
            assert!(
                (f64::from(density) - exact_density).abs() / exact_density < 0.01,
                "{percent}%: density {density} for {exact_density}"
            );
            assert!(
                (f64::from(px_100_dp) - wanted).abs() <= wanted * 0.01 + 0.5,
                "{percent}%: {px_100_dp} pixels for {wanted}"
            );
            assert_eq!(dip_to_px(100, dpi), percent, "{percent}%");
        }
    }

    #[test]
    fn fr4_a_size_keeps_its_density_independent_size_across_densities() {
        assert_eq!(rescale((548, 974), 160, 320), (1096, 1948));
        assert_eq!(rescale((1096, 1948), 320, 160), (548, 974));
        assert_eq!(rescale((548, 974), 160, 240), (822, 1461));
        assert_eq!(rescale((548, 974), 0, 320), (548, 974));
        assert_eq!(
            rescale((1, 1), 320, 160),
            (1, 1),
            "never shrinks to nothing"
        );
    }

    #[test]
    fn fr4_resizing_the_window_asks_the_guest_for_its_client_size() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        let now = Instant::now();
        assert_eq!(sizing.view_resized((640, 480), now), live_resize(640, 480));
        // The same size again, or the size the task already has, asks nothing.
        assert_eq!(sizing.view_resized((640, 480), now), None);
        let mut fresh = WindowSizing::new(&task(400, 300, 130));
        assert_eq!(fresh.view_resized((400, 300), now), None);
        assert_eq!(
            fresh.view_resized((0, 300), now),
            None,
            "a minimised window"
        );
    }

    #[test]
    fn fr4_the_guest_is_never_asked_for_more_than_it_can_draw() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        assert_eq!(sizing.max_client_size(), Some((1920, 1080)));
        assert_eq!(
            sizing.view_resized((2560, 900), Instant::now()),
            live_resize(1920, 900)
        );
    }

    #[test]
    fn fr4_the_window_does_not_jump_while_the_guest_catches_up() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        let now = Instant::now();
        sizing.dragging(true, now);
        sizing.view_resized((500, 300), now);
        sizing.view_resized((600, 300), now);
        // The guest reports the sizes on the way; the window keeps the user's size.
        assert_eq!(sizing.guest_updated(&task(500, 300, 130)), None);
        assert!(sizing.waiting());
        assert_eq!(sizing.guest_updated(&task(600, 300, 130)), None);
        assert!(!sizing.waiting());
        assert_eq!(sizing.dragging(false, now), resize(600, 300));
        assert_eq!(sizing.tick(now + SETTLE * 2), None);
    }

    #[test]
    fn fr4_a_size_the_guest_does_not_take_is_not_left_stretched() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        let start = Instant::now();
        sizing.view_resized((700, 300), start);
        // The guest gives the task another size than the one asked for.
        assert_eq!(sizing.guest_updated(&task(650, 300, 130)), None);
        assert_eq!(sizing.tick(start + SETTLE / 2), None, "it may still come");
        assert_eq!(sizing.tick(start + SETTLE), Some((650, 300)));
        // Once the window has taken it, nothing more is asked.
        assert_eq!(sizing.view_resized((650, 300), start + SETTLE), None);
        assert_eq!(sizing.tick(start + SETTLE * 3), None);
    }

    #[test]
    fn fr4_the_window_never_resizes_itself_under_a_drag() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        let start = Instant::now();
        sizing.dragging(true, start);
        sizing.view_resized((700, 300), start);
        assert_eq!(sizing.guest_updated(&task(650, 300, 130)), None);
        assert_eq!(sizing.tick(start + SETTLE * 5), None);
        // The guest has SETTLE from the end of the drag.
        let end = start + SETTLE * 5;
        assert_eq!(sizing.dragging(false, end), resize(700, 300));
        assert_eq!(sizing.tick(end + SETTLE / 2), None);
        assert_eq!(sizing.tick(end + SETTLE), Some((650, 300)));
    }

    #[test]
    fn fr4_a_task_the_guest_resizes_on_its_own_resizes_its_window() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        assert_eq!(sizing.guest_updated(&task(800, 600, 130)), Some((800, 600)));
        // The window reporting that size asks nothing back.
        assert_eq!(sizing.view_resized((800, 600), Instant::now()), None);
        // A task that only moves leaves its window alone.
        let mut moved = task(800, 600, 130);
        moved.bounds.left += 100;
        moved.bounds.right += 100;
        assert_eq!(sizing.guest_updated(&moved), None);
    }

    #[test]
    fn fr4_moving_to_a_monitor_of_another_scale_asks_for_its_density_first() {
        let mut sizing = WindowSizing::new(&task(548, 974, 130));
        let now = Instant::now();
        assert_eq!(sizing.dpi_changed(96), None, "already right at 100%");
        assert_eq!(sizing.dpi_changed(192), density(262));
        assert_eq!(sizing.dpi_changed(192), None, "asked once");
        // The window manager doubles the window; the size follows the density.
        assert_eq!(
            sizing.view_resized((1096, 1948), now),
            live_resize(1096, 1080)
        );
        // And back.
        assert_eq!(sizing.dpi_changed(96), density(130));
    }

    #[test]
    fn fr4_the_first_scale_a_window_reports_is_where_it_opened() {
        let mut sizing = WindowSizing::new(&task(400, 300, 130));
        assert_eq!(sizing.dpi_reported(192), (Some((806, 605)), density(262)));
        // The window reports its new size; the task is asked for it.
        assert_eq!(
            sizing.view_resized((806, 605), Instant::now()),
            live_resize(806, 605)
        );
        // A later report is a move.
        assert_eq!(sizing.dpi_reported(96), (None, density(130)));
        let mut at_100 = WindowSizing::new(&task(400, 300, 130));
        assert_eq!(at_100.dpi_reported(96), (None, None));
        assert_eq!(at_100.task_size(), ((400, 300), Some((1920, 1080)), None));
    }

    #[test]
    fn fr4_a_window_opens_at_its_monitors_scale() {
        let mut on_200 = WindowSizing::new(&task(548, 974, 130));
        assert_eq!(on_200.opened(192), ((607, 1080), density(262)));
        let mut on_100 = WindowSizing::new(&task(548, 974, 130));
        assert_eq!(on_100.opened(96), ((548, 974), None));
        let mut on_150 = WindowSizing::new(&task(400, 300, 130));
        assert_eq!(on_150.opened(144), ((603, 452), density(196)));
        // A task whose density the agent could not read keeps its size.
        let mut unknown = WindowSizing::new(&task(400, 300, 0));
        assert_eq!(unknown.opened(192), ((400, 300), density(262)));
    }
}
