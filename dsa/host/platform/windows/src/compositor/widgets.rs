//! The widgets of the user's Android, as borderless windows on the user's desktop.
//!
//! The window manager watches the widgets placed on the user's desktop over the local API and
//! shows each one in a window of its own ([`sync`]). A widget window is a layered popup
//! window that belongs to the desktop: it is owned by the shell's desktop window, so it stays
//! when the user shows the desktop (Win+D), it is kept below every other window, and as a
//! tool window that never takes the focus it is neither in the taskbar nor in Alt+Tab
//! ([`window_styles`]).
//!
//! The window shows the widget's latest frame, a PNG image with an alpha channel, so a widget
//! with rounded corners or a see-through background looks as it does on a phone. While the
//! pointer is over the widget, or the window has the keyboard, a small round "More" button
//! fades in at its top right corner, inside its rounded corners ([`more_button`]), as on the
//! cards of the Windows 11 Widgets board; clicking it opens the widget's menu under it, as a
//! right click opens it at the pointer ([`super::widget_menu`]).
//!
//! A press, a move and a release inside the widget go to the widget's view in Android, so its
//! buttons and lists work and an app it opens shows as a window. The window is moved by
//! dragging its More button, or by pressing anywhere on the widget and holding still for
//! [`HOLD_TO_MOVE`] before dragging, as a widget is moved on a phone's home screen; a press
//! that moves sooner is the widget's own (a list scrolls), and a tap stays a tap
//! ([`Gesture`]). The window is resized by its edges ([`hit_test`]). When the user lets go,
//! the service is told where the window is and how large, in its monitor's pixels and
//! density; the service remembers it and tells Android, which draws the widget at that size
//! and tells the widget's app its size in dp, so the app can pick the layout that fits.
//!
//! The window manager starts at sign-in, and it starts Android then when it is not running
//! ([`starts_android_at_sign_in`]): the first app opened after the computer starts does not
//! wait for Android to boot, and the widgets the service remembers come back.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::host_api::HostApiClient;
use dsa_host_core::session::SessionState;
use dsa_host_core::widgets::{self, Placement, Widget, WidgetEvent, WidgetFrame};
use dsa_protocol::host_api::types::PointerAction;
use dsa_transport::Connector;

use crate::compositor::log::{note, CompositorLogger};

/// What the window manager calls itself to the service.
const BUILD: &str = "dsa-wm widgets";

/// How long a request to the service is waited for.
const ANSWER: Duration = Duration::from_secs(5);

/// How long to wait before connecting again after the service could not be reached.
const RETRY: Duration = Duration::from_secs(1);

/// How long the watch waits for the service before it passes on what the windows reported.
const POLL: Duration = Duration::from_millis(15);

/// The width of the edges a widget window is resized by, in device-independent pixels.
pub const BORDER_DIP: i32 = 6;

/// The diameter of a widget window's More button, in device-independent pixels.
pub const MORE_BUTTON_DIP: u32 = 28;

/// How far the More button sits from the window's top and right edges, which keeps it
/// inside every corner radius an Android widget is drawn with (up to 41 dp) and off the edges
/// that resize the window.
pub const MORE_BUTTON_MARGIN_DIP: u32 = 8;

/// The size of the More button's glyph.
pub const MORE_GLYPH_DIP: u32 = 16;

/// How long the More button takes to fade in or out: WinUI's `ControlFastAnimationDuration`.
pub const FADE: Duration = Duration::from_millis(167);

/// How long a press on the widget is held still before dragging it moves the window, as a
/// long press does on a phone's home screen.
pub const HOLD_TO_MOVE: Duration = Duration::from_millis(500);

/// The host's density at a scale of 100%.
const HOST_BASE_DPI: u32 = 96;

/// `dp` at a monitor of `dpi` (96 at a scale of 100%), in pixels, rounded to the nearest.
/// A dp is one device-independent pixel.
pub fn px_of_dip(dp: u32, dpi: u32) -> i32 {
    let dpi = if dpi == 0 { HOST_BASE_DPI } else { dpi };
    ((u64::from(dp) * u64::from(dpi) + u64::from(HOST_BASE_DPI) / 2) / u64::from(HOST_BASE_DPI))
        .min(i32::MAX as u64) as i32
}

/// `px` pixels at a monitor of `dpi`, in dp, rounded to the nearest.
pub fn dip_of_px(px: i32, dpi: u32) -> u32 {
    let dpi = if dpi == 0 { HOST_BASE_DPI } else { dpi };
    let px = px.max(0) as u64;
    ((px * u64::from(HOST_BASE_DPI) + u64::from(dpi) / 2) / u64::from(dpi)) as u32
}

/// Android's density for a monitor of `dpi`: 160 at a scale of 100%.
pub fn android_density(dpi: u32) -> u32 {
    let dpi = if dpi == 0 { HOST_BASE_DPI } else { dpi };
    (dpi * widgets::BASE_DPI + HOST_BASE_DPI / 2) / HOST_BASE_DPI
}

/// The size of a widget's window on a monitor of `dpi`, in pixels.
pub fn window_size(placement: &Placement, dpi: u32) -> (i32, i32) {
    (
        px_of_dip(placement.width_dp, dpi).max(1),
        px_of_dip(placement.height_dp, dpi).max(1),
    )
}

/// The placement of a window at `x`, `y` of `width` by `height` pixels on a monitor of `dpi`.
pub fn placement_of(x: i32, y: i32, width: i32, height: i32, dpi: u32) -> Placement {
    Placement {
        x,
        y,
        width_dp: dip_of_px(width, dpi).max(1),
        height_dp: dip_of_px(height, dpi).max(1),
    }
}

/// The smallest a widget's window may be made on a monitor of `dpi`, in pixels.
pub fn min_window_size(provider: &widgets::WidgetProvider, dpi: u32) -> (i32, i32) {
    let (width, height) = widgets::clamp_size(provider, 0, 0);
    (px_of_dip(width, dpi), px_of_dip(height, dpi))
}

/// The window style and extended style of a widget window: a popup with no frame, layered
/// so it can be partly see-through, a tool window so it is neither in the taskbar nor in
/// Alt+Tab, and one that never takes the focus, so clicking it leaves the user's window
/// active.
pub fn window_styles() -> (u32, u32) {
    const WS_POPUP: u32 = 0x8000_0000;
    const WS_EX_TOOLWINDOW: u32 = 0x0000_0080;
    const WS_EX_LAYERED: u32 = 0x0008_0000;
    const WS_EX_NOACTIVATE: u32 = 0x0800_0000;
    (
        WS_POPUP,
        WS_EX_TOOLWINDOW | WS_EX_LAYERED | WS_EX_NOACTIVATE,
    )
}

/// How long a widget window waits for its first image from Android, after it opened, before
/// the window manager logs that it has none. Android draws a widget within a second of being
/// told its size, so a window still without an image after this shows only its
/// placeholder, and the log says so.
pub const NO_FRAME: Duration = Duration::from_secs(10);

/// How often the widget windows are checked for an image that never came.
pub const NO_FRAME_CHECK: Duration = Duration::from_secs(5);

/// Whether a window that has waited for an image since `waiting_since` (None: it has one) is
/// overdue at `now`, and not logged yet.
pub fn frame_overdue(waiting_since: Option<Instant>, noted: bool, now: Instant) -> bool {
    match waiting_since {
        Some(since) => !noted && now.saturating_duration_since(since) >= NO_FRAME,
        None => false,
    }
}

/// The corner radius of a widget window's placeholder, in device-independent pixels.
const PLACEHOLDER_RADIUS_DIP: u32 = 12;

/// What a widget window shows until its first image from Android comes, or while the image
/// it has is all clear: a dark grey card with a lighter edge and rounded corners, as
/// premultiplied BGRA rows of `width` by `height` pixels on a monitor of `dpi`. It is mostly
/// opaque, so the user sees where the widget is and can grab it even when Android has not
/// drawn it yet; a window that showed nothing would look like no widget at all.
pub fn placeholder(width: usize, height: usize, dpi: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; width * height * 4];
    let radius = (px_of_dip(PLACEHOLDER_RADIUS_DIP, dpi).max(1) as usize)
        .min(width / 2)
        .min(height / 2) as i64;
    let edge = px_of_dip(1, dpi).max(1) as i64;
    let (w, h) = (width as i64, height as i64);
    // Premultiplied BGRA: #383838 at 0xd8 inside, #909090 at 0xe8 on the edge.
    let fill = [0x2f, 0x2f, 0x2f, 0xd8];
    let border = [0x83, 0x83, 0x83, 0xe8];
    for y in 0..h {
        for x in 0..w {
            // How far into a rounded corner's square the pixel is, 0 outside the squares.
            let cx = if x < radius {
                radius - x
            } else if x >= w - radius {
                x - (w - radius - 1)
            } else {
                0
            };
            let cy = if y < radius {
                radius - y
            } else if y >= h - radius {
                y - (h - radius - 1)
            } else {
                0
            };
            let in_corner = cx > 0 && cy > 0;
            if in_corner && cx * cx + cy * cy > radius * radius {
                continue;
            }
            let inner = (radius - edge).max(0);
            let on_edge = x < edge
                || y < edge
                || x >= w - edge
                || y >= h - edge
                || (in_corner && cx * cx + cy * cy > inner * inner);
            let at = ((y * w + x) * 4) as usize;
            pixels[at..at + 4].copy_from_slice(if on_edge { &border } else { &fill });
        }
    }
    pixels
}

/// Whether premultiplied BGRA `pixels` show nothing: every alpha is zero. A widget always
/// draws something, so such an image is a drawing that went wrong, and the window shows its
/// placeholder instead of a see-through hole.
pub fn all_clear(pixels: &[u8]) -> bool {
    pixels.chunks_exact(4).all(|pixel| pixel[3] == 0)
}

/// Where a widget window goes in the z-order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZPlace {
    /// Right above the desktop: below the window this names, which is right above it.
    After(isize),
    /// It is right above the desktop already, or the desktop is the top window; the z-order
    /// stays.
    Keep,
    /// No desktop was found: the bottom of the z-order.
    Bottom,
}

/// The z-order a widget window `own` takes: right above the desktop's own window (`desktop`,
/// the one that holds the desktop icons), with `above_desktop` the window right above that
/// now. That is below every other window, but not the bottom of the z-order: once a
/// wallpaper slideshow or "show desktop" has run, the shell moves the icons into a window of
/// its own above `Progman` that paints the wallpaper, and a window at the bottom is hidden
/// under it.
pub fn z_place(desktop: Option<isize>, above_desktop: Option<isize>, own: isize) -> ZPlace {
    match (desktop, above_desktop) {
        (None, _) => ZPlace::Bottom,
        (Some(_), Some(above)) if above == own => ZPlace::Keep,
        (Some(_), Some(above)) => ZPlace::After(above),
        (Some(_), None) => ZPlace::Keep,
    }
}

/// How often the widget windows are checked to be right above the desktop. The shell
/// restacks its own windows without telling anyone: "show desktop", a wallpaper slideshow
/// and an Explorer restart raise the window that holds the desktop icons and paints the
/// wallpaper, and a widget window that only fixed its place when it moved itself stayed under
/// it, drawn but hidden behind the desktop.
pub const RESTACK_CHECK: Duration = Duration::from_millis(500);

/// One window below a widget window, as [`right_above_desktop`] reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Below {
    pub hwnd: isize,
    /// Whether it shows over part of the widget window: visible, not minimized and
    /// overlapping it.
    pub covers: bool,
}

/// Whether a widget window sits where it belongs, given the windows below it in z-order from
/// the nearest down (`below`): the desktop's own window (`desktop`) comes before any other
/// window that overlaps it. Other widget windows (`ours`) and windows that do not overlap it
/// may be in between. False when the desktop is not below it at all, which means the desktop
/// is drawn over it, and when an app's window is below it, which it would hide.
pub fn right_above_desktop(below: &[Below], desktop: isize, ours: &[isize]) -> bool {
    for window in below {
        if window.hwnd == desktop {
            return true;
        }
        if window.covers && !ours.contains(&window.hwnd) {
            return false;
        }
    }
    false
}

/// Where in a widget window a point is, which says what dragging it does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit {
    /// Inside the widget: input goes to Android.
    Client,
    /// The More button: a click opens the menu and a drag moves the window.
    More,
    Left,
    Right,
    Top,
    Bottom,
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

/// What `x`, `y` (from the window's top left corner) hits in a window of `width` by `height`
/// pixels on a monitor of `dpi`. Only the edges the widget may be resized along resize it;
/// there is no band along the top, so all of the widget but its More button is the widget's.
pub fn hit_test(x: i32, y: i32, width: i32, height: i32, dpi: u32, resize_mode: u32) -> Hit {
    let border = px_of_dip(BORDER_DIP as u32, dpi).max(1);
    let horizontal = resize_mode & widgets::RESIZE_HORIZONTAL != 0;
    let vertical = resize_mode & widgets::RESIZE_VERTICAL != 0;
    let left = horizontal && x < border;
    let right = horizontal && x >= width - border;
    let top = vertical && y < border;
    let bottom = vertical && y >= height - border;
    match (left, right, top, bottom) {
        (true, _, true, _) => Hit::TopLeft,
        (_, true, true, _) => Hit::TopRight,
        (true, _, _, true) => Hit::BottomLeft,
        (_, true, _, true) => Hit::BottomRight,
        (true, ..) => Hit::Left,
        (_, true, ..) => Hit::Right,
        (_, _, true, _) => Hit::Top,
        (.., true) => Hit::Bottom,
        _ if on_more_button(x, y, width, height, dpi) => Hit::More,
        _ => Hit::Client,
    }
}

/// A circle in a window's pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Circle {
    pub cx: f32,
    pub cy: f32,
    pub radius: f32,
}

impl Circle {
    pub fn contains(&self, x: f32, y: f32) -> bool {
        let (dx, dy) = (x - self.cx, y - self.cy);
        dx * dx + dy * dy <= self.radius * self.radius
    }

    /// The square of whole pixels around it: left, top, right and bottom.
    pub fn bounds(&self) -> (i32, i32, i32, i32) {
        (
            (self.cx - self.radius).floor() as i32,
            (self.cy - self.radius).floor() as i32,
            (self.cx + self.radius).ceil() as i32,
            (self.cy + self.radius).ceil() as i32,
        )
    }
}

/// The More button of a window of `width` by `height` pixels on a monitor of `dpi`: a circle
/// of [`MORE_BUTTON_DIP`] in its top right corner, [`MORE_BUTTON_MARGIN_DIP`] from the edges.
/// A widget too small for it gets a smaller one that still fits.
pub fn more_button(width: i32, height: i32, dpi: u32) -> Circle {
    let side = width.min(height).max(1) as f32;
    let diameter = (px_of_dip(MORE_BUTTON_DIP, dpi) as f32).min((side - 2.0).max(1.0));
    let room = ((side - diameter) / 2.0).max(0.0);
    let margin = (px_of_dip(MORE_BUTTON_MARGIN_DIP, dpi) as f32).min(room);
    let radius = diameter / 2.0;
    Circle {
        cx: width as f32 - margin - radius,
        cy: margin + radius,
        radius,
    }
}

/// Whether the pixel at `x`, `y` is on the More button of a window of `width` by `height`:
/// inside its circle, not merely its square.
pub fn on_more_button(x: i32, y: i32, width: i32, height: i32, dpi: u32) -> bool {
    more_button(width, height, dpi).contains(x as f32 + 0.5, y as f32 + 0.5)
}

/// Whether `circle` lies wholly inside a window of `width` by `height` whose corners are
/// rounded to `radius`, as the widget's own image is.
pub fn inside_rounded_window(circle: &Circle, width: i32, height: i32, radius: f32) -> bool {
    let (w, h, rho) = (width as f32, height as f32, circle.radius);
    let edges = circle.cx - rho >= 0.0
        && circle.cy - rho >= 0.0
        && circle.cx + rho <= w
        && circle.cy + rho <= h;
    if rho >= radius {
        return edges;
    }
    // A rounded rectangle is the rectangle `radius` inside it, grown by `radius`.
    let dx = (radius - circle.cx).max(circle.cx - (w - radius)).max(0.0);
    let dy = (radius - circle.cy).max(circle.cy - (h - radius)).max(0.0);
    edges && (dx * dx + dy * dy).sqrt() + rho <= radius + 0.01
}

/// How much of the More button shows, from 0 (none) to 1, while it fades from `from` to `to`
/// since `since`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Fade {
    pub from: f32,
    pub to: f32,
    pub since: Instant,
}

impl Fade {
    /// A button that is not shown.
    pub fn hidden(now: Instant) -> Fade {
        Fade {
            from: 0.0,
            to: 0.0,
            since: now,
        }
    }

    pub fn level(&self, now: Instant) -> f32 {
        let done = now.saturating_duration_since(self.since).as_secs_f32() / FADE.as_secs_f32();
        self.from + (self.to - self.from) * done.clamp(0.0, 1.0)
    }

    pub fn done(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.since) >= FADE
    }

    /// The fade towards `shown` from where this one is at `now`; this one when it goes
    /// there already.
    pub fn toward(self, shown: bool, now: Instant) -> Fade {
        if (self.to > 0.5) == shown {
            return self;
        }
        let to = if shown { 1.0 } else { 0.0 };
        Fade {
            from: self.level(now),
            to,
            since: now,
        }
    }
}

/// Whether the More button shows: while the pointer is over the widget, while the window has
/// the keyboard, while its menu is open, and while it is pressed or moving the window.
pub fn more_button_shown(hover: bool, focused: bool, menu_open: bool, gesture: &Gesture) -> bool {
    hover
        || focused
        || menu_open
        || matches!(gesture, Gesture::Button { .. } | Gesture::Moving { .. })
}

/// What a press on a widget window is doing.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Gesture {
    Idle,
    /// Pressed inside the widget, at `down` on the screen, `since` then; Android has the
    /// press. Once it `slid` past the drag threshold it is the widget's own drag.
    Widget {
        down: (i32, i32),
        since: Instant,
        slid: bool,
    },
    /// Pressed on the More button, at `down` on the screen.
    Button {
        down: (i32, i32),
    },
    /// Moving the window, the pointer `grab` pixels from its top left corner.
    Moving {
        grab: (i32, i32),
    },
}

/// What a step of a gesture asks of the window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Step {
    Nothing,
    /// The pointer goes to Android as this action.
    Android(PointerAction),
    /// The More button is pressed, or no longer pressed.
    Button,
    /// The More button was clicked: its menu opens.
    OpenMenu,
    /// The window starts moving, to this top left corner; Android's press is cancelled
    /// first when `cancel`.
    StartMove {
        cancel: bool,
        to: (i32, i32),
    },
    /// The window moves to this top left corner.
    MoveTo((i32, i32)),
    /// The move ended: the service is told where the window is.
    EndMove,
}

/// Whether `at` is past the drag threshold (`threshold` pixels each way) from `down`.
fn beyond(down: (i32, i32), at: (i32, i32), threshold: (i32, i32)) -> bool {
    (at.0 - down.0).abs() > threshold.0 || (at.1 - down.1).abs() > threshold.1
}

impl Gesture {
    /// The button went down at `at` on the screen, on the More button or not.
    pub fn down(&mut self, on_button: bool, at: (i32, i32), now: Instant) -> Step {
        if on_button {
            *self = Gesture::Button { down: at };
            Step::Button
        } else {
            *self = Gesture::Widget {
                down: at,
                since: now,
                slid: false,
            };
            Step::Android(PointerAction::Down)
        }
    }

    /// The pointer moved to `at` on the screen, the window's top left corner at `origin`.
    pub fn moved(&mut self, at: (i32, i32), origin: (i32, i32), threshold: (i32, i32)) -> Step {
        match *self {
            Gesture::Idle => Step::Nothing,
            Gesture::Widget { down, since, slid } => {
                if !slid && beyond(down, at, threshold) {
                    *self = Gesture::Widget {
                        down,
                        since,
                        slid: true,
                    };
                }
                Step::Android(PointerAction::Move)
            }
            Gesture::Button { down } if beyond(down, at, threshold) => {
                let grab = (down.0 - origin.0, down.1 - origin.1);
                *self = Gesture::Moving { grab };
                Step::StartMove {
                    cancel: false,
                    to: (at.0 - grab.0, at.1 - grab.1),
                }
            }
            Gesture::Button { .. } => Step::Nothing,
            Gesture::Moving { grab } => Step::MoveTo((at.0 - grab.0, at.1 - grab.1)),
        }
    }

    /// The press has been held until `now`, the pointer at `at`: one held still for
    /// [`HOLD_TO_MOVE`] starts moving the window, and Android's press is cancelled.
    pub fn held(&mut self, at: (i32, i32), origin: (i32, i32), now: Instant) -> Step {
        let Gesture::Widget { since, slid, .. } = *self else {
            return Step::Nothing;
        };
        if slid || now.saturating_duration_since(since) < HOLD_TO_MOVE {
            return Step::Nothing;
        }
        let grab = (at.0 - origin.0, at.1 - origin.1);
        *self = Gesture::Moving { grab };
        Step::StartMove {
            cancel: true,
            to: origin,
        }
    }

    /// The button went up, on the More button or not.
    pub fn up(&mut self, on_button: bool) -> Step {
        match std::mem::replace(self, Gesture::Idle) {
            Gesture::Idle => Step::Nothing,
            Gesture::Widget { .. } => Step::Android(PointerAction::Up),
            Gesture::Button { .. } if on_button => Step::OpenMenu,
            Gesture::Button { .. } => Step::Button,
            Gesture::Moving { .. } => Step::EndMove,
        }
    }

    /// The window lost the mouse before the button went up.
    pub fn cancel(&mut self) -> Step {
        match std::mem::replace(self, Gesture::Idle) {
            Gesture::Idle => Step::Nothing,
            Gesture::Widget { .. } => Step::Android(PointerAction::Cancel),
            Gesture::Button { .. } => Step::Button,
            Gesture::Moving { .. } => Step::EndMove,
        }
    }
}

/// The More button drawn over a widget's pixels.
#[derive(Debug, Clone, Copy)]
pub struct ButtonLook<'a> {
    /// How much of it shows, from 0 to 1, as it fades.
    pub level: f32,
    pub hover: bool,
    pub pressed: bool,
    pub palette: super::fluent::ButtonPalette,
    /// Its glyph, centred in it.
    pub glyph: Option<&'a super::fluent_menu::Mask>,
}

/// `color` at `level` (0 to 1) of its alpha.
fn faded(color: super::fluent::Argb, level: f32) -> super::fluent::Argb {
    let alpha = (((color >> 24) as f32) * level.clamp(0.0, 1.0)).round() as u32;
    (alpha.min(255) << 24) | (color & 0x00FF_FFFF)
}

/// Draws the More button of a window of `width` by `height` pixels on a monitor of `dpi`
/// over its premultiplied BGRA `pixels`: a subtle fill with a hairline edge, lighter or
/// darker while the pointer is on it or presses it, and the "More" glyph.
pub fn draw_more_button(
    pixels: &mut [u8],
    width: usize,
    height: usize,
    dpi: u32,
    look: &ButtonLook<'_>,
) {
    if look.level <= 0.0 {
        return;
    }
    let circle = more_button(width as i32, height as i32, dpi);
    let mut canvas = super::fluent::Canvas {
        width,
        height,
        pixels,
    };
    let (r, d) = (circle.radius, circle.radius * 2.0);
    let (x, y) = (circle.cx - r, circle.cy - r);
    let palette = &look.palette;
    canvas.fill_rounded(x, y, d, d, r, faded(palette.fill, look.level));
    let state = if look.pressed {
        Some(palette.pressed)
    } else if look.hover {
        Some(palette.hover)
    } else {
        None
    };
    if let Some(overlay) = state {
        canvas.fill_rounded(x, y, d, d, r, faded(overlay, look.level));
    }
    let hairline = px_of_dip(1, dpi).max(1) as f32;
    canvas.stroke_rounded(x, y, d, d, r, hairline, faded(palette.border, look.level));
    if let Some(glyph) = look.glyph {
        let color = if look.pressed {
            palette.glyph_pressed
        } else {
            palette.glyph
        };
        let left = (circle.cx - glyph.width as f32 / 2.0).round() as i32;
        let top = (circle.cy - glyph.height as f32 / 2.0).round() as i32;
        let (w, h) = (glyph.width, glyph.height);
        canvas.draw_mask(left, top, w, h, &glyph.cover, color, look.level);
    }
}

/// The size of the More button's glyph box for a window of `width` by `height` on `dpi`.
pub fn more_glyph_size(width: i32, height: i32, dpi: u32) -> usize {
    let circle = more_button(width, height, dpi);
    let wanted = px_of_dip(MORE_GLYPH_DIP, dpi) as f32;
    (wanted.min(circle.radius * 2.0 * 0.6).round() as usize).max(1)
}

/// A rectangle in screen pixels: left, top, right and bottom.
pub type ScreenRect = (i32, i32, i32, i32);

/// Where a window of `width` by `height` pixels asked for at `x`, `y` goes so the user can
/// reach it: where asked when enough of it is on a monitor's work area, else moved into the
/// first work area, as after a monitor was unplugged.
pub fn reachable_origin(
    x: i32,
    y: i32,
    width: i32,
    height: i32,
    areas: &[ScreenRect],
) -> (i32, i32) {
    const ENOUGH: i32 = 48;
    let on_screen = areas.iter().any(|&(left, top, right, bottom)| {
        let w = (x + width).min(right) - x.max(left);
        let h = (y + height).min(bottom) - y.max(top);
        w >= ENOUGH.min(width) && h >= ENOUGH.min(height) && y >= top - ENOUGH / 2
    });
    if on_screen || areas.is_empty() {
        return (x, y);
    }
    let (left, top, right, bottom) = areas[0];
    (
        x.clamp(left, (right - width).max(left)),
        y.clamp(top, (bottom - height).max(top)),
    )
}

/// A point in a window of `window` pixels, in the pixels of a frame of `frame` pixels, which
/// differ while a resize has not reached Android yet.
pub fn to_frame(x: i32, y: i32, window: (i32, i32), frame: (u32, u32)) -> (f32, f32) {
    let scale = |v: i32, from: i32, to: u32| {
        if from <= 0 {
            v as f32
        } else {
            v as f32 * to as f32 / from as f32
        }
    };
    (
        scale(x, window.0, frame.0).clamp(0.0, frame.0 as f32),
        scale(y, window.1, frame.1).clamp(0.0, frame.1 as f32),
    )
}

/// What the window manager asks of the widget windows.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowCommand {
    /// Open the widget's window, or move it to its placement.
    Place(Widget),
    /// Show what the widget shows now.
    Frame(WidgetFrame),
    /// Close the widget's window.
    Remove(i32),
}

/// What the widget windows report to the window manager.
#[derive(Debug, Clone, PartialEq)]
pub enum WindowReport {
    /// The window is at `placement`, `width` by `height` pixels on a monitor of
    /// `density_dpi` (Android's density: 160 at a scale of 100%).
    Placed {
        widget_id: i32,
        placement: Placement,
        width: u32,
        height: u32,
        density_dpi: u32,
    },
    /// Pointer input in the widget, in its frame's pixels.
    Pointer {
        widget_id: i32,
        action: PointerAction,
        x: f32,
        y: f32,
    },
    /// The user chose Remove in the window's menu.
    Remove { widget_id: i32 },
}

/// The widget windows, as the watch drives them: the window manager's are Win32 windows on
/// a thread of their own ([`WindowsWidgetWindows`]); a test's records what it is asked.
pub trait WidgetWindows {
    fn command(&mut self, command: WindowCommand);
    /// What the windows reported since the last call.
    fn reports(&mut self) -> Vec<WindowReport>;
}

/// The command a change to the widgets is for the windows.
pub fn command_of(event: WidgetEvent) -> Option<WindowCommand> {
    match event {
        WidgetEvent::Placed(widget) => Some(WindowCommand::Place(widget)),
        WidgetEvent::Frame(frame) => Some(WindowCommand::Frame(frame)),
        WidgetEvent::Removed { widget_id } => Some(WindowCommand::Remove(widget_id)),
        WidgetEvent::Listed(_) => None,
    }
}

/// Moves still to send are dropped when a newer one of the same widget follows, so a slow
/// service never makes a drag lag behind.
pub fn coalesce(reports: Vec<WindowReport>) -> Vec<WindowReport> {
    let mut out: Vec<WindowReport> = Vec::with_capacity(reports.len());
    for report in reports {
        if let WindowReport::Pointer {
            widget_id,
            action: PointerAction::Move,
            ..
        } = &report
        {
            if let Some(WindowReport::Pointer {
                widget_id: last,
                action: PointerAction::Move,
                ..
            }) = out.last()
            {
                if last == widget_id {
                    out.pop();
                }
            }
        }
        out.push(report);
    }
    out
}

/// Passes what the windows reported to the service through `client`. A report the service
/// refuses is logged; the rest go on.
fn send_reports(
    client: &mut HostApiClient,
    reports: Vec<WindowReport>,
    logger: Option<&CompositorLogger>,
) -> Result<(), String> {
    for report in coalesce(reports) {
        let result = match report {
            WindowReport::Placed {
                widget_id,
                placement,
                width,
                height,
                density_dpi,
            } => client.place_widget(widget_id, placement, width, height, density_dpi),
            WindowReport::Pointer {
                widget_id,
                action,
                x,
                y,
            } => client.widget_pointer(widget_id, action, 0, x, y),
            WindowReport::Remove { widget_id } => {
                note(
                    logger,
                    &format!("removing widget {widget_id} from its menu"),
                );
                client.remove_widget(widget_id)
            }
        };
        match result {
            Ok(()) => {}
            Err(dsa_host_core::host_api::ClientError::Service { detail, .. }) => {
                note(logger, &format!("a widget's window: {detail}"));
            }
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

/// Shows the widgets of the user's Android until `stop`, reconnecting to the service
/// whenever the connection ends.
pub fn sync(
    mut connector: Box<dyn Connector>,
    windows: &mut dyn WidgetWindows,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let mut last_error = String::new();
    let mut started_android = false;
    let mut shown: HashSet<i32> = HashSet::new();
    while !stop.load(Ordering::Relaxed) {
        let stream = HostApiClient::connect(&mut *connector, BUILD, ANSWER)
            .and_then(|client| client.watch_widgets());
        let requests = HostApiClient::connect(&mut *connector, BUILD, ANSWER);
        let (mut stream, mut requests) = match (stream, requests) {
            (Ok(stream), Ok(requests)) => (stream, requests),
            (Err(error), _) | (_, Err(error)) => {
                let error = error.to_string();
                if error != last_error {
                    note(
                        logger.as_deref(),
                        &format!("widgets cannot reach the service: {error}"),
                    );
                    last_error = error;
                }
                thread::sleep(RETRY);
                continue;
            }
        };
        last_error.clear();
        // Android starts with the user's sign-in, so no app launch waits for it to boot, and
        // the widgets the service remembers come back once it runs.
        if !started_android {
            started_android = true;
            match requests.status() {
                Ok(status) if starts_android_at_sign_in(status.state) => {
                    note(logger.as_deref(), "starting Android at sign-in");
                    if let Err(why) = requests.start() {
                        note(
                            logger.as_deref(),
                            &format!("Android did not start at sign-in: {why}"),
                        );
                    }
                }
                Ok(_) => {}
                Err(why) => note(logger.as_deref(), &format!("widgets: {why}")),
            }
        }
        // The windows of widgets that went while the connection was down close.
        let current: Vec<i32> = stream.current().iter().map(|w| w.widget_id).collect();
        for gone in shown
            .iter()
            .copied()
            .filter(|id| !current.contains(id))
            .collect::<Vec<_>>()
        {
            shown.remove(&gone);
            windows.command(WindowCommand::Remove(gone));
        }
        for widget in stream.current().to_vec() {
            shown.insert(widget.widget_id);
            windows.command(WindowCommand::Place(widget));
        }
        while !stop.load(Ordering::Relaxed) {
            if let Err(error) = send_reports(&mut requests, windows.reports(), logger.as_deref()) {
                note(
                    logger.as_deref(),
                    &format!("a widget's window did not reach the service: {error}"),
                );
                break;
            }
            match stream.next(POLL) {
                Ok(Some(event)) => {
                    match &event {
                        WidgetEvent::Placed(widget) => {
                            shown.insert(widget.widget_id);
                        }
                        WidgetEvent::Removed { widget_id } => {
                            shown.remove(widget_id);
                        }
                        _ => {}
                    }
                    if let Some(command) = command_of(event) {
                        windows.command(command);
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    note(
                        logger.as_deref(),
                        &format!("the widgets' connection to the service ended: {error}"),
                    );
                    break;
                }
            }
        }
    }
    for id in shown {
        windows.command(WindowCommand::Remove(id));
    }
}

/// The widget windows: Win32 layered windows on a thread of their own, which the watch
/// drives through a channel.
#[cfg(windows)]
pub struct WindowsWidgetWindows {
    commands: std::sync::mpsc::Sender<WindowCommand>,
    reports: std::sync::mpsc::Receiver<WindowReport>,
    controller: Arc<std::sync::atomic::AtomicIsize>,
    thread: Option<thread::JoinHandle<()>>,
}

#[cfg(windows)]
impl WindowsWidgetWindows {
    /// Starts the windows' thread.
    pub fn open(logger: Option<Arc<CompositorLogger>>) -> Result<WindowsWidgetWindows, String> {
        let (commands, command_rx) = std::sync::mpsc::channel();
        let (report_tx, reports) = std::sync::mpsc::channel();
        let (ready_tx, ready) = std::sync::mpsc::channel();
        let controller = Arc::new(std::sync::atomic::AtomicIsize::new(0));
        let slot = Arc::clone(&controller);
        let thread = thread::Builder::new()
            .name("dsa-wm-widget-windows".into())
            .spawn(move || win::run(slot, command_rx, report_tx, ready_tx, logger))
            .map_err(|e| format!("could not start the widgets' thread: {e}"))?;
        match ready.recv() {
            Ok(Ok(())) => Ok(WindowsWidgetWindows {
                commands,
                reports,
                controller,
                thread: Some(thread),
            }),
            Ok(Err(why)) => {
                let _ = thread.join();
                Err(why)
            }
            Err(_) => {
                let _ = thread.join();
                Err("the widgets' thread ended before it was ready".into())
            }
        }
    }
}

#[cfg(windows)]
impl WidgetWindows for WindowsWidgetWindows {
    fn command(&mut self, command: WindowCommand) {
        if self.commands.send(command).is_ok() {
            win::wake(self.controller.load(Ordering::SeqCst));
        }
    }

    fn reports(&mut self) -> Vec<WindowReport> {
        self.reports.try_iter().collect()
    }
}

#[cfg(windows)]
impl Drop for WindowsWidgetWindows {
    fn drop(&mut self) {
        win::close(self.controller.load(Ordering::SeqCst));
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(windows)]
mod win {
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::ptr::{null, null_mut};
    use std::sync::atomic::{AtomicIsize, Ordering};
    use std::sync::mpsc::{Receiver, Sender};
    use std::sync::{Arc, Mutex};

    use dsa_host_core::widgets::{Placement, Widget, WidgetFrame};
    use dsa_protocol::host_api::types::PointerAction;
    use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, RECT, SIZE, WPARAM};
    use windows_sys::Win32::Graphics::Gdi::{
        CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, GetDC, GetMonitorInfoW,
        MonitorFromWindow, ReleaseDC, SelectObject, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
        BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, DIB_RGB_COLORS, MONITORINFO,
        MONITOR_DEFAULTTONEAREST,
    };
    use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
    use windows_sys::Win32::UI::HiDpi::{
        GetDpiForWindow, GetSystemMetricsForDpi, SetThreadDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
        ReleaseCapture, SetCapture, SetFocus, TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT,
        VK_RETURN, VK_SPACE,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, FindWindowExW,
        FindWindowW, GetCursorPos, GetMessageW, GetWindow, GetWindowRect, KillTimer, LoadCursorW,
        PostMessageW, PostQuitMessage, RegisterClassExW, RegisterWindowMessageW, SetCursor,
        SetForegroundWindow, SetTimer, SetWindowLongPtrW, SetWindowPos, ShowWindow,
        TranslateMessage, UpdateLayeredWindow, GWLP_HWNDPARENT, GW_HWNDPREV, HTBOTTOM,
        HTBOTTOMLEFT, HTBOTTOMRIGHT, HTCLIENT, HTLEFT, HTRIGHT, HTTOP, HTTOPLEFT, HTTOPRIGHT,
        HWND_BOTTOM, HWND_MESSAGE, IDC_ARROW, IDC_SIZEALL, MA_NOACTIVATE, MINMAXINFO, MSG,
        SM_CXDRAG, SM_CYDRAG, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE, SWP_NOZORDER,
        SW_SHOWNOACTIVATE, ULW_ALPHA, WINDOWPOS, WM_APP, WM_CAPTURECHANGED, WM_CLOSE,
        WM_CONTEXTMENU, WM_DESTROY, WM_DPICHANGED, WM_ENTERSIZEMOVE, WM_EXITSIZEMOVE,
        WM_GETMINMAXINFO, WM_GETOBJECT, WM_KEYDOWN, WM_KILLFOCUS, WM_LBUTTONDOWN, WM_LBUTTONUP,
        WM_MOUSEACTIVATE, WM_MOUSEMOVE, WM_NCDESTROY, WM_NCHITTEST, WM_SETFOCUS, WM_SETTINGCHANGE,
        WM_SIZE, WM_SYSCOLORCHANGE, WM_THEMECHANGED, WM_TIMER, WM_WINDOWPOSCHANGING, WNDCLASSEXW,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::{IsIconic, IsWindowVisible, GW_HWNDNEXT};

    use std::time::Instant;

    use super::{
        android_density, draw_more_button, frame_overdue, hit_test, min_window_size, more_button,
        more_button_shown, more_glyph_size, on_more_button, placement_of, px_of_dip,
        reachable_origin, right_above_desktop, to_frame, window_size, window_styles, z_place,
        Below, ButtonLook, Fade, Gesture, Hit, Step, WindowCommand, WindowReport, ZPlace,
        HOLD_TO_MOVE, MORE_GLYPH_DIP, NO_FRAME, NO_FRAME_CHECK, RESTACK_CHECK,
    };
    use crate::compositor::fluent::text::{self, Align, ICON_FACES};
    use crate::compositor::fluent::{button_palette, glyph, read_theme, Theme};
    use crate::compositor::fluent_menu::{Anchor, Mask};
    use crate::compositor::log::{note, CompositorLogger};
    use crate::compositor::widget_automation::{self, WidgetView, WM_WIDGET_FOCUS, WM_WIDGET_MORE};
    use crate::compositor::widget_menu::{self, MenuChoice};
    use windows::Win32::UI::Accessibility::IRawElementProviderSimple;

    /// The message that tells the controller window commands are waiting.
    const WM_COMMANDS: u32 = WM_APP + 1;

    /// The pointer left the window (`WM_MOUSELEAVE`, which windows-sys keeps with the common
    /// controls).
    const WM_MOUSELEAVE: u32 = 0x02A3;

    struct Window {
        widget: Widget,
        frame: Option<WidgetFrame>,
        /// The last image that had something in it, which the window keeps showing when
        /// Android sends one with nothing in it.
        shown: Option<WidgetFrame>,
        /// The widget's image at the window's size before the More button is drawn over it,
        /// so the button is drawn again (as it fades) without decoding the image again.
        base: Option<(i32, i32, Vec<u8>)>,
        hover: bool,
        /// Whether Android has a press of the widget.
        pressed: bool,
        gesture: Gesture,
        /// Whether the pointer is over the More button.
        over_button: bool,
        /// Whether the window has the keyboard.
        focused: bool,
        menu_open: bool,
        fade: Fade,
        /// Since when the window waits for its first image; None once one came.
        waiting_since: Option<Instant>,
        /// Whether the wait was logged already.
        noted: bool,
        /// What UI Automation reads of the window, and the provider it was answered with.
        view: Arc<Mutex<WidgetView>>,
        provider: Option<IRawElementProviderSimple>,
    }

    /// The window's timer that turns a held press into a move.
    const HOLD_TIMER: usize = 3;

    /// The window's timer that draws the More button as it fades.
    const FADE_TIMER: usize = 4;

    /// How often the fade is drawn, in milliseconds.
    const FADE_STEP_MS: u32 = 16;

    /// The controller's timer that checks for windows still without an image.
    const NO_FRAME_TIMER: usize = 1;

    /// The controller's timer that keeps the windows right above the desktop.
    const RESTACK_TIMER: usize = 2;

    /// What the last drawing of a window handed the system, so a test can tell a window that
    /// shows something from one that shows nothing.
    #[cfg(test)]
    #[derive(Debug, Clone, Copy)]
    pub(super) struct Presented {
        pub hwnd: isize,
        /// Whether UpdateLayeredWindow took the picture.
        pub committed: bool,
        pub width: i32,
        pub height: i32,
        /// How many of its pixels are at least half opaque.
        pub opaque: usize,
        /// How many of its pixels are see-through: an alpha of at most 1, which keeps them
        /// clickable.
        pub clear: usize,
        /// Whether the window was shown already when it got the picture.
        pub visible: bool,
    }

    #[cfg(test)]
    pub(super) static PRESENTED: std::sync::Mutex<Vec<Presented>> =
        std::sync::Mutex::new(Vec::new());

    struct State {
        commands: Receiver<WindowCommand>,
        reports: Sender<WindowReport>,
        windows: HashMap<isize, Window>,
        by_id: HashMap<i32, isize>,
        class: Vec<u16>,
        taskbar_created: u32,
        logger: Option<Arc<CompositorLogger>>,
        decoder: Option<super::png::Decoder>,
        /// The theme the More button is drawn in.
        theme: Theme,
        /// The More button's glyph at a DPI and a size.
        more_glyph: Option<(u32, usize, Mask)>,
    }

    thread_local! {
        static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
    }

    fn wide(text: &str) -> Vec<u16> {
        text.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
        STATE.with(|state| match state.try_borrow_mut() {
            Ok(mut state) => state.as_mut().map(f),
            Err(_) => None,
        })
    }

    pub(super) fn wake(controller: isize) {
        if controller != 0 {
            // SAFETY: posts to the controller window, which may have gone; then it fails.
            unsafe { PostMessageW(controller as HWND, WM_COMMANDS, 0, 0) };
        }
    }

    pub(super) fn close(controller: isize) {
        if controller != 0 {
            // SAFETY: as above.
            unsafe { PostMessageW(controller as HWND, WM_CLOSE, 0, 0) };
        }
    }

    /// The shell's desktop window, which widget windows belong to so they stay on the
    /// desktop when the user shows it.
    fn desktop_owner() -> HWND {
        let class = wide("Progman");
        // SAFETY: a NUL-terminated class name.
        unsafe { FindWindowW(class.as_ptr(), null()) }
    }

    fn lo(l: LPARAM) -> i32 {
        (l & 0xffff) as u16 as i16 as i32
    }

    fn hi(l: LPARAM) -> i32 {
        ((l >> 16) & 0xffff) as u16 as i16 as i32
    }

    fn rect_of(hwnd: HWND) -> RECT {
        let mut rect = RECT {
            left: 0,
            top: 0,
            right: 0,
            bottom: 0,
        };
        // SAFETY: a window of this thread and a rectangle to fill.
        unsafe { GetWindowRect(hwnd, &mut rect) };
        rect
    }

    fn dpi_of(hwnd: HWND) -> u32 {
        // SAFETY: a window of this thread.
        match unsafe { GetDpiForWindow(hwnd) } {
            0 => 96,
            dpi => dpi,
        }
    }

    fn work_area(hwnd: HWND) -> Option<super::ScreenRect> {
        // SAFETY: a window of this thread, and a structure of the right size to fill.
        unsafe {
            let monitor = MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST);
            let mut info: MONITORINFO = std::mem::zeroed();
            info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
            if GetMonitorInfoW(monitor, &mut info) == 0 {
                return None;
            }
            let w = info.rcWork;
            Some((w.left, w.top, w.right, w.bottom))
        }
    }

    /// Opens the controller window, says whether it did, and runs the windows' messages
    /// until it is closed.
    pub(super) fn run(
        slot: Arc<AtomicIsize>,
        commands: Receiver<WindowCommand>,
        reports: Sender<WindowReport>,
        ready: Sender<Result<(), String>>,
        logger: Option<Arc<CompositorLogger>>,
    ) {
        // Every window of this thread follows its monitor's scale.
        // SAFETY: changes this thread's DPI awareness only.
        unsafe { SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2) };
        let _com = crate::start_menu::ComApartment::enter();
        let decoder = match super::png::Decoder::new() {
            Ok(decoder) => Some(decoder),
            Err(why) => {
                note(logger.as_deref(), &format!("widgets show no image: {why}"));
                None
            }
        };
        let controller_class = wide(&format!("DsaWmWidgets_{}", std::process::id()));
        let class = wide(&format!("DsaWmWidget_{}", std::process::id()));
        // SAFETY: the module handle of this process.
        let instance = unsafe { GetModuleHandleW(null()) };
        for (name, proc_) in [
            (&controller_class, controller_proc as WndProc),
            (&class, widget_proc as WndProc),
        ] {
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: 0,
                lpfnWndProc: Some(proc_),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: instance,
                hIcon: null_mut(),
                // SAFETY: a system cursor.
                hCursor: unsafe { LoadCursorW(null_mut(), IDC_ARROW) },
                hbrBackground: null_mut(),
                lpszMenuName: null(),
                lpszClassName: name.as_ptr(),
                hIconSm: null_mut(),
            };
            // SAFETY: the names outlive the classes, which live as long as the process.
            unsafe { RegisterClassExW(&wc) };
        }
        let taskbar = wide("TaskbarCreated");
        // SAFETY: a NUL-terminated message name.
        let taskbar_created = unsafe { RegisterWindowMessageW(taskbar.as_ptr()) };
        STATE.with(|state| {
            *state.borrow_mut() = Some(State {
                commands,
                reports,
                windows: HashMap::new(),
                by_id: HashMap::new(),
                class,
                taskbar_created,
                logger,
                decoder,
                theme: read_theme(),
                more_glyph: None,
            })
        });
        // SAFETY: a message-only window, never shown.
        let controller = unsafe {
            CreateWindowExW(
                0,
                controller_class.as_ptr(),
                controller_class.as_ptr(),
                0,
                0,
                0,
                0,
                0,
                HWND_MESSAGE,
                null_mut(),
                instance,
                null(),
            )
        };
        if controller.is_null() {
            let _ = ready.send(Err(format!(
                "could not open the widgets' window: {}",
                std::io::Error::last_os_error()
            )));
            return;
        }
        // SAFETY: a timer of the controller window, killed when it closes.
        unsafe {
            SetTimer(
                controller,
                NO_FRAME_TIMER,
                NO_FRAME_CHECK.as_millis() as u32,
                None,
            );
            SetTimer(
                controller,
                RESTACK_TIMER,
                RESTACK_CHECK.as_millis() as u32,
                None,
            );
        }
        slot.store(controller as isize, Ordering::SeqCst);
        let _ = ready.send(Ok(()));
        // SAFETY: the usual message loop of this thread's windows.
        unsafe {
            let mut msg: MSG = std::mem::zeroed();
            while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        slot.store(0, Ordering::SeqCst);
        STATE.with(|state| *state.borrow_mut() = None);
    }

    type WndProc = unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT;

    unsafe extern "system" fn controller_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        match msg {
            WM_COMMANDS => {
                apply_commands();
                0
            }
            WM_TIMER if wparam == NO_FRAME_TIMER => {
                note_missing_frames();
                0
            }
            WM_TIMER if wparam == RESTACK_TIMER => {
                keep_above_desktop();
                0
            }
            WM_CLOSE => {
                let windows = with_state(|s| s.windows.drain().collect::<Vec<_>>());
                for (hwnd, window) in windows.unwrap_or_default() {
                    widget_automation::release(hwnd, &window.view, window.provider);
                    // SAFETY: windows of this thread.
                    unsafe { DestroyWindow(hwnd as HWND) };
                }
                // SAFETY: the controller is this thread's, and so are its timers.
                unsafe {
                    KillTimer(hwnd, NO_FRAME_TIMER);
                    KillTimer(hwnd, RESTACK_TIMER);
                    DestroyWindow(hwnd);
                }
                0
            }
            WM_DESTROY => {
                // SAFETY: ends this thread's message loop.
                unsafe { PostQuitMessage(0) };
                0
            }
            // SAFETY: the default handling of every other message.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    fn apply_commands() {
        loop {
            let Some(Some(command)) = with_state(|s| s.commands.try_recv().ok()) else {
                return;
            };
            match command {
                WindowCommand::Place(widget) => place(widget),
                WindowCommand::Frame(frame) => {
                    let hwnd = with_state(|s| {
                        let hwnd = *s.by_id.get(&frame.widget_id)?;
                        let window = s.windows.get_mut(&hwnd)?;
                        if window.noted {
                            note(
                                s.logger.as_deref(),
                                &format!(
                                    "widget {}: its first image came, {}x{} pixels",
                                    frame.widget_id, frame.width, frame.height
                                ),
                            );
                        }
                        window.waiting_since = None;
                        window.noted = false;
                        window.frame = Some(frame);
                        window.base = None;
                        Some(hwnd)
                    })
                    .flatten();
                    if let Some(hwnd) = hwnd {
                        present(hwnd as HWND);
                    }
                }
                WindowCommand::Remove(widget_id) => {
                    let gone = with_state(|s| {
                        let hwnd = s.by_id.remove(&widget_id)?;
                        Some((hwnd, s.windows.remove(&hwnd)))
                    })
                    .flatten();
                    if let Some((hwnd, window)) = gone {
                        if let Some(window) = window {
                            widget_automation::release(hwnd, &window.view, window.provider);
                        }
                        // SAFETY: a window of this thread.
                        unsafe { DestroyWindow(hwnd as HWND) };
                    }
                }
            }
        }
    }

    /// Logs each widget window that has waited [`NO_FRAME`] for its first image from Android,
    /// once, so a widget that shows nothing is visible in the log.
    fn note_missing_frames() {
        let now = Instant::now();
        with_state(|s| {
            for (hwnd, window) in s.windows.iter_mut() {
                if !frame_overdue(window.waiting_since, window.noted, now) {
                    continue;
                }
                window.noted = true;
                let waited = window
                    .waiting_since
                    .map(|since| now.saturating_duration_since(since).as_secs())
                    .unwrap_or(NO_FRAME.as_secs());
                let rect = rect_of(*hwnd as HWND);
                note(
                    s.logger.as_deref(),
                    &format!(
                        "widget {} ({}) has had no image from Android for {waited} s; its window of {}x{} pixels shows only its placeholder. The service's and the agent's logs say why",
                        window.widget.widget_id,
                        window.widget.provider.provider,
                        rect.right - rect.left,
                        rect.bottom - rect.top,
                    ),
                );
            }
        });
    }

    /// The desktop's own window: the shell's window that holds the desktop icons, which a
    /// wallpaper slideshow or "show desktop" moves out of `Progman` into a `WorkerW`.
    pub(super) fn desktop_window() -> Option<HWND> {
        let progman = desktop_owner();
        let defview = wide("SHELLDLL_DefView");
        let worker = wide("WorkerW");
        // SAFETY: walks the top-level windows by class; the names outlive the calls.
        unsafe {
            if !progman.is_null()
                && !FindWindowExW(progman, null_mut(), defview.as_ptr(), null()).is_null()
            {
                return Some(progman);
            }
            let mut after: HWND = null_mut();
            loop {
                after = FindWindowExW(null_mut(), after, worker.as_ptr(), null());
                if after.is_null() {
                    break;
                }
                if !FindWindowExW(after, null_mut(), defview.as_ptr(), null()).is_null() {
                    return Some(after);
                }
            }
        }
        (!progman.is_null()).then_some(progman)
    }

    /// Puts every widget window that the shell left under the desktop, or above an app's
    /// window, right above the desktop again ([`right_above_desktop`]). The shell restacks
    /// its desktop window without moving the widget windows, so they hear nothing of it.
    fn keep_above_desktop() {
        let Some(desktop) = desktop_window() else {
            return;
        };
        let ours: Vec<isize> =
            with_state(|s| s.windows.keys().copied().collect()).unwrap_or_default();
        for &own in &ours {
            let rect = rect_of(own as HWND);
            let mut below = Vec::new();
            // SAFETY: walks the z-order down from a window of this thread; a window that goes
            // meanwhile ends the walk.
            unsafe {
                let mut next = GetWindow(own as HWND, GW_HWNDNEXT);
                while !next.is_null() && below.len() < 4096 {
                    let other = rect_of(next);
                    let overlaps = other.left < rect.right
                        && rect.left < other.right
                        && other.top < rect.bottom
                        && rect.top < other.bottom;
                    below.push(Below {
                        hwnd: next as isize,
                        covers: overlaps && IsWindowVisible(next) != 0 && IsIconic(next) == 0,
                    });
                    next = GetWindow(next, GW_HWNDNEXT);
                }
            }
            if !right_above_desktop(&below, desktop as isize, &ours) {
                // SAFETY: a window of this thread; WM_WINDOWPOSCHANGING puts it right above
                // the desktop.
                unsafe {
                    SetWindowPos(
                        own as HWND,
                        HWND_BOTTOM,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                    );
                }
            }
        }
    }

    /// Where `hwnd` goes in the z-order: right above the desktop ([`z_place`]).
    fn z_place_of(hwnd: HWND) -> ZPlace {
        let desktop = desktop_window();
        // SAFETY: reads the window right above the desktop's.
        let above = desktop.map(|d| unsafe { GetWindow(d, GW_HWNDPREV) });
        z_place(
            desktop.map(|d| d as isize),
            above.filter(|a| !a.is_null()).map(|a| a as isize),
            hwnd as isize,
        )
    }

    /// Opens the widget's window at its placement, or moves the open one there, and tells
    /// the service the window's size, which is how Android learns what size to draw at.
    fn place(widget: Widget) {
        let existing = with_state(|s| s.by_id.get(&widget.widget_id).copied()).flatten();
        let hwnd = match existing {
            Some(hwnd) => {
                with_state(|s| {
                    if let Some(window) = s.windows.get_mut(&hwnd) {
                        window.widget = widget.clone();
                    }
                });
                hwnd as HWND
            }
            None => {
                let Some(class) = with_state(|s| s.class.clone()) else {
                    return;
                };
                let (style, ex_style) = window_styles();
                let (width, height) = window_size(&widget.placement, 96);
                // SAFETY: a popup window owned by the desktop; the names outlive the call.
                let hwnd = unsafe {
                    CreateWindowExW(
                        ex_style,
                        class.as_ptr(),
                        class.as_ptr(),
                        style,
                        widget.placement.x,
                        widget.placement.y,
                        width,
                        height,
                        desktop_owner(),
                        null_mut(),
                        GetModuleHandleW(null()),
                        null(),
                    )
                };
                if hwnd.is_null() {
                    with_state(|s| {
                        note(
                            s.logger.as_deref(),
                            &format!(
                                "the window of widget {} did not open: {}",
                                widget.widget_id,
                                std::io::Error::last_os_error()
                            ),
                        )
                    });
                    return;
                }
                // A finger's press reaches the widget at once, and a finger held on it moves
                // it as a held mouse button does, rather than becoming a right click.
                // SAFETY: the window was just made on this thread.
                unsafe { crate::app_window::pointer::deliver_contacts_at_once(hwnd) };
                with_state(|s| {
                    s.by_id.insert(widget.widget_id, hwnd as isize);
                    s.windows.insert(
                        hwnd as isize,
                        Window {
                            widget: widget.clone(),
                            frame: None,
                            shown: None,
                            base: None,
                            hover: false,
                            pressed: false,
                            gesture: Gesture::Idle,
                            over_button: false,
                            focused: false,
                            menu_open: false,
                            fade: Fade::hidden(Instant::now()),
                            waiting_since: Some(Instant::now()),
                            noted: false,
                            view: Arc::new(Mutex::new(WidgetView::default())),
                            provider: None,
                        },
                    );
                });
                hwnd
            }
        };
        let dpi = dpi_of(hwnd);
        let (width, height) = window_size(&widget.placement, dpi);
        let (x, y) = match work_area(hwnd) {
            Some(area) => reachable_origin(
                widget.placement.x,
                widget.placement.y,
                width,
                height,
                &[area],
            ),
            None => (widget.placement.x, widget.placement.y),
        };
        // SAFETY: a window of this thread; it goes right above the desktop, where
        // WM_WINDOWPOSCHANGING puts it.
        unsafe { SetWindowPos(hwnd, HWND_BOTTOM, x, y, width, height, SWP_NOACTIVATE) };
        // A layered window shows nothing at all until it is given a picture, so it gets its
        // placeholder before it is shown, never after.
        present(hwnd);
        // SAFETY: as above.
        unsafe { ShowWindow(hwnd, SW_SHOWNOACTIVATE) };
        keep_above_desktop();
        report_placement(hwnd);
    }

    fn report_placement(hwnd: HWND) {
        let rect = rect_of(hwnd);
        let dpi = dpi_of(hwnd);
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        with_state(|s| {
            let Some(window) = s.windows.get(&(hwnd as isize)) else {
                return;
            };
            let placement = placement_of(rect.left, rect.top, width, height, dpi);
            let _ = s.reports.send(WindowReport::Placed {
                widget_id: window.widget.widget_id,
                placement,
                width: width.max(1) as u32,
                height: height.max(1) as u32,
                density_dpi: android_density(dpi),
            });
        });
    }

    /// The widget's image at `width` by `height` pixels: its latest frame, scaled to its
    /// window, or the last one that had something in it when Android sent one with nothing,
    /// or the placeholder until the first one comes.
    fn base_pixels(
        window: &mut Window,
        decoder: Option<&super::png::Decoder>,
        logger: Option<&CompositorLogger>,
        width: i32,
        height: i32,
        dpi: u32,
    ) -> Vec<u8> {
        let decode = |frame: &WidgetFrame| match decoder {
            Some(decoder) => decoder
                .decode(&frame.png, width as u32, height as u32)
                .map_err(|why| note(logger, &format!("a widget's image: {why}")))
                .ok(),
            None => None,
        };
        let mut decoded = window.frame.as_ref().and_then(decode);
        if decoded.as_deref().is_some_and(super::all_clear) {
            // A widget whose app stopped drawing keeps its last image; the placeholder is
            // only for a widget that has never shown one.
            note(
                logger,
                &format!(
                    "widget {}: Android sent an image with nothing in it; {}",
                    window.widget.widget_id,
                    if window.shown.is_some() {
                        "keeping the last one"
                    } else {
                        "showing the placeholder"
                    }
                ),
            );
            window.frame = window.shown.clone();
            decoded = window.frame.as_ref().and_then(decode);
        } else if decoded.is_some() {
            window.shown = window.frame.clone();
        }
        decoded.unwrap_or_else(|| super::placeholder(width as usize, height as usize, dpi))
    }

    /// The More button's glyph for a window of `width` by `height` on `dpi`, rasterized once
    /// for each DPI and size.
    fn more_glyph_of(
        cache: &mut Option<(u32, usize, Mask)>,
        width: i32,
        height: i32,
        dpi: u32,
    ) -> Option<&Mask> {
        let size = more_glyph_size(width, height, dpi);
        let fresh = !matches!(cache, Some((at, side, _)) if (*at, *side) == (dpi, size));
        if fresh {
            let em = px_of_dip(MORE_GLYPH_DIP, dpi).min(size as i32);
            let symbol = glyph::MORE.to_string();
            let cover = text::mask(&symbol, &ICON_FACES, em, (size, size), Align::Center);
            *cache = cover.map(|cover| {
                let mask = Mask {
                    width: size,
                    height: size,
                    cover,
                };
                (dpi, size, mask)
            });
        }
        cache.as_ref().map(|(_, _, mask)| mask)
    }

    /// Draws the widget's image with the More button over it while the button shows. Pixels
    /// the widget leaves clear stay just visible enough to be clicked, so the whole window
    /// takes the pointer.
    fn present(hwnd: HWND) {
        let rect = rect_of(hwnd);
        let (width, height) = (
            (rect.right - rect.left).max(1),
            (rect.bottom - rect.top).max(1),
        );
        let dpi = dpi_of(hwnd);
        let now = Instant::now();
        let pixels = with_state(|s| {
            let State {
                windows,
                decoder,
                logger,
                theme,
                more_glyph,
                ..
            } = s;
            let window = windows.get_mut(&(hwnd as isize))?;
            let cached = match &window.base {
                Some((w, h, pixels)) if (*w, *h) == (width, height) => Some(pixels.clone()),
                _ => None,
            };
            let mut pixels = match cached {
                Some(pixels) => pixels,
                None => {
                    let (decoder, logger) = (decoder.as_ref(), logger.as_deref());
                    let pixels = base_pixels(window, decoder, logger, width, height, dpi);
                    window.base = Some((width, height, pixels.clone()));
                    pixels
                }
            };
            let level = window.fade.level(now);
            let glyph = if level > 0.0 {
                more_glyph_of(more_glyph, width, height, dpi)
            } else {
                None
            };
            let look = ButtonLook {
                level,
                hover: window.over_button,
                pressed: matches!(window.gesture, Gesture::Button { .. }),
                palette: button_palette(theme),
                glyph,
            };
            finish(&mut pixels, width as usize, height as usize, dpi, &look);
            Some(pixels)
        })
        .flatten();
        let Some(pixels) = pixels else {
            return;
        };
        let mut committed = false;
        let mut failure = None;
        // SAFETY: a DIB section of the window's size, filled from `pixels`, selected into a
        // memory DC for UpdateLayeredWindow and freed after.
        unsafe {
            let screen = GetDC(null_mut());
            let memory = CreateCompatibleDC(screen);
            let mut info: BITMAPINFO = std::mem::zeroed();
            info.bmiHeader = BITMAPINFOHEADER {
                biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                biWidth: width,
                biHeight: -height,
                biPlanes: 1,
                biBitCount: 32,
                biCompression: BI_RGB,
                biSizeImage: 0,
                biXPelsPerMeter: 0,
                biYPelsPerMeter: 0,
                biClrUsed: 0,
                biClrImportant: 0,
            };
            let mut bits: *mut core::ffi::c_void = null_mut();
            let bitmap = CreateDIBSection(memory, &info, DIB_RGB_COLORS, &mut bits, null_mut(), 0);
            if !bitmap.is_null() && !bits.is_null() {
                std::ptr::copy_nonoverlapping(pixels.as_ptr(), bits as *mut u8, pixels.len());
                let old = SelectObject(memory, bitmap);
                let size = SIZE {
                    cx: width,
                    cy: height,
                };
                let source = POINT { x: 0, y: 0 };
                let blend = BLENDFUNCTION {
                    BlendOp: AC_SRC_OVER as u8,
                    BlendFlags: 0,
                    SourceConstantAlpha: 255,
                    AlphaFormat: AC_SRC_ALPHA as u8,
                };
                committed = UpdateLayeredWindow(
                    hwnd,
                    screen,
                    null(),
                    &size,
                    memory,
                    &source,
                    0,
                    &blend,
                    ULW_ALPHA,
                ) != 0;
                if !committed {
                    failure = Some(std::io::Error::last_os_error());
                }
                SelectObject(memory, old);
                DeleteObject(bitmap);
            } else {
                failure = Some(std::io::Error::last_os_error());
            }
            DeleteDC(memory);
            ReleaseDC(null_mut(), screen);
        }
        if let Some(why) = failure {
            // Without a picture a layered window is not drawn at all, so this is logged.
            with_state(|s| {
                let id = s.windows.get(&(hwnd as isize)).map(|w| w.widget.widget_id);
                note(
                    s.logger.as_deref(),
                    &format!(
                        "widget {}: its window of {width}x{height} pixels could not be drawn: {why}",
                        id.unwrap_or_default()
                    ),
                );
            });
        }
        record(hwnd, committed, width, height, &pixels);
        sync_view(hwnd);
    }

    /// The More button's square on the screen: left, top, right and bottom.
    fn button_on_screen(hwnd: HWND) -> (i32, i32, i32, i32) {
        let rect = rect_of(hwnd);
        let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
        let (left, top, right, bottom) = more_button(width, height, dpi_of(hwnd)).bounds();
        (
            rect.left + left,
            rect.top + top,
            rect.left + right,
            rect.top + bottom,
        )
    }

    /// Writes what UI Automation reads of the window now.
    fn sync_view(hwnd: HWND) {
        let (left, top, right, bottom) = button_on_screen(hwnd);
        let text = widget_menu::display_text();
        with_state(|s| {
            let Some(window) = s.windows.get(&(hwnd as isize)) else {
                return;
            };
            let mut view = window.view.lock().unwrap_or_else(|e| e.into_inner());
            view.alive = true;
            view.hwnd = hwnd as isize;
            view.name = widget_menu::menu_name(&text, &window.widget.provider);
            view.button_name = text.more.to_string();
            view.button = (left, top, right - left, bottom - top);
            view.focused = window.focused;
        });
    }

    /// Fades the More button in or out as the window's state asks now, and draws it.
    fn update_button(hwnd: HWND) {
        let key = hwnd as isize;
        let now = Instant::now();
        let fading = with_state(|s| {
            let w = s.windows.get_mut(&key)?;
            let shown = more_button_shown(w.hover, w.focused, w.menu_open, &w.gesture);
            let before = w.fade;
            w.fade = w.fade.toward(shown, now);
            Some(w.fade != before)
        })
        .flatten()
        .unwrap_or(false);
        if fading {
            // SAFETY: a timer of this window, killed once the fade is done.
            unsafe { SetTimer(hwnd, FADE_TIMER, FADE_STEP_MS, None) };
        }
        present(hwnd);
    }

    /// How far the pointer moves before a press is a drag, at `dpi`.
    fn drag_threshold(dpi: u32) -> (i32, i32) {
        // SAFETY: plain reads of the system's metrics.
        let x = unsafe { GetSystemMetricsForDpi(SM_CXDRAG, dpi) };
        // SAFETY: as above.
        let y = unsafe { GetSystemMetricsForDpi(SM_CYDRAG, dpi) };
        (x.max(1), y.max(1))
    }

    /// Runs a step of the window's gesture.
    fn gesture(hwnd: HWND, step: impl FnOnce(&mut Gesture) -> Step) -> Step {
        let key = hwnd as isize;
        let done = with_state(|s| s.windows.get_mut(&key).map(|w| step(&mut w.gesture)));
        done.flatten().unwrap_or(Step::Nothing)
    }

    fn set_pressed(hwnd: HWND, pressed: bool) {
        with_state(|s| {
            if let Some(w) = s.windows.get_mut(&(hwnd as isize)) {
                w.pressed = pressed;
            }
        });
    }

    /// Moves the window's top left corner to `x`, `y` on the screen.
    fn move_to(hwnd: HWND, (x, y): (i32, i32)) {
        // SAFETY: a window of this thread; it keeps its size and its place in the z-order.
        unsafe {
            SetWindowPos(
                hwnd,
                null_mut(),
                x,
                y,
                0,
                0,
                SWP_NOACTIVATE | SWP_NOSIZE | SWP_NOZORDER,
            );
        }
    }

    /// Does what a step of the window's gesture asks.
    fn do_step(hwnd: HWND, step: Step, lparam: LPARAM) {
        match step {
            Step::Nothing => {}
            Step::Android(action) => {
                if action == PointerAction::Down {
                    set_pressed(hwnd, true);
                }
                pointer(hwnd, action, lparam);
                if matches!(action, PointerAction::Up | PointerAction::Cancel) {
                    set_pressed(hwnd, false);
                }
            }
            Step::Button => update_button(hwnd),
            Step::OpenMenu => {
                update_button(hwnd);
                open_menu(hwnd, None, false);
            }
            Step::StartMove { cancel, to } => {
                if cancel {
                    pointer(hwnd, PointerAction::Cancel, 0);
                    set_pressed(hwnd, false);
                }
                // SAFETY: a system cursor, shown while the window moves.
                unsafe { SetCursor(LoadCursorW(null_mut(), IDC_SIZEALL)) };
                move_to(hwnd, to);
                update_button(hwnd);
            }
            Step::MoveTo(to) => move_to(hwnd, to),
            Step::EndMove => {
                report_placement(hwnd);
                update_button(hwnd);
            }
        }
    }

    #[cfg(test)]
    fn record(hwnd: HWND, committed: bool, width: i32, height: i32, pixels: &[u8]) {
        if let Ok(mut presented) = PRESENTED.lock() {
            presented.push(Presented {
                hwnd: hwnd as isize,
                committed,
                width,
                height,
                opaque: pixels.chunks_exact(4).filter(|p| p[3] >= 0x80).count(),
                clear: pixels.chunks_exact(4).filter(|p| p[3] <= 1).count(),
                // SAFETY: a window of this thread.
                visible: unsafe { IsWindowVisible(hwnd) } != 0,
            });
        }
    }

    #[cfg(not(test))]
    fn record(_: HWND, _: bool, _: i32, _: i32, _: &[u8]) {}

    /// Keeps every pixel clickable and draws the More button over the widget while it shows.
    fn finish(pixels: &mut [u8], width: usize, height: usize, dpi: u32, button: &ButtonLook<'_>) {
        for pixel in pixels.chunks_exact_mut(4) {
            if pixel[3] == 0 {
                pixel[3] = 1;
            }
        }
        draw_more_button(pixels, width, height, dpi, button);
    }

    unsafe extern "system" fn widget_proc(
        hwnd: HWND,
        msg: u32,
        wparam: WPARAM,
        lparam: LPARAM,
    ) -> LRESULT {
        let key = hwnd as isize;
        match msg {
            WM_NCHITTEST => {
                let rect = rect_of(hwnd);
                let (x, y) = (lo(lparam) - rect.left, hi(lparam) - rect.top);
                let resize_mode =
                    with_state(|s| s.windows.get(&key).map(|w| w.widget.provider.resize_mode))
                        .flatten()
                        .unwrap_or(0);
                let hit = hit_test(
                    x,
                    y,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    dpi_of(hwnd),
                    resize_mode,
                );
                (match hit {
                    Hit::Client | Hit::More => HTCLIENT,
                    Hit::Left => HTLEFT,
                    Hit::Right => HTRIGHT,
                    Hit::Top => HTTOP,
                    Hit::Bottom => HTBOTTOM,
                    Hit::TopLeft => HTTOPLEFT,
                    Hit::TopRight => HTTOPRIGHT,
                    Hit::BottomLeft => HTBOTTOMLEFT,
                    Hit::BottomRight => HTBOTTOMRIGHT,
                }) as LRESULT
            }
            WM_MOUSEACTIVATE => MA_NOACTIVATE as LRESULT,
            WM_WINDOWPOSCHANGING => {
                // Kept right above the desktop, below every other window, whatever raises it.
                let pos = lparam as *mut WINDOWPOS;
                // SAFETY: the system passes a WINDOWPOS it reads back.
                unsafe {
                    if !pos.is_null() && (*pos).flags & SWP_NOZORDER == 0 {
                        match z_place_of(hwnd) {
                            ZPlace::After(above) => (*pos).hwndInsertAfter = above as HWND,
                            ZPlace::Keep => (*pos).flags |= SWP_NOZORDER,
                            ZPlace::Bottom => (*pos).hwndInsertAfter = HWND_BOTTOM,
                        }
                    }
                }
                0
            }
            WM_GETMINMAXINFO => {
                let min = with_state(|s| {
                    s.windows
                        .get(&key)
                        .map(|w| min_window_size(&w.widget.provider, dpi_of(hwnd)))
                })
                .flatten();
                if let Some((width, height)) = min {
                    let info = lparam as *mut MINMAXINFO;
                    // SAFETY: the system passes a MINMAXINFO it reads back.
                    unsafe {
                        if !info.is_null() {
                            (*info).ptMinTrackSize = POINT {
                                x: width,
                                y: height,
                            };
                        }
                    }
                }
                0
            }
            WM_SIZE => {
                present(hwnd);
                0
            }
            WM_ENTERSIZEMOVE => 0,
            WM_EXITSIZEMOVE => {
                report_placement(hwnd);
                0
            }
            WM_DPICHANGED => {
                let suggested = lparam as *const RECT;
                // SAFETY: the system passes the rectangle it suggests for the new scale.
                unsafe {
                    if !suggested.is_null() {
                        let r = *suggested;
                        SetWindowPos(
                            hwnd,
                            null_mut(),
                            r.left,
                            r.top,
                            r.right - r.left,
                            r.bottom - r.top,
                            SWP_NOACTIVATE | SWP_NOZORDER,
                        );
                    }
                }
                report_placement(hwnd);
                0
            }
            WM_MOUSEMOVE => {
                let rect = rect_of(hwnd);
                let (x, y) = (lo(lparam), hi(lparam));
                let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
                let dpi = dpi_of(hwnd);
                let over = on_more_button(x, y, width, height, dpi);
                let (at, origin) = ((rect.left + x, rect.top + y), (rect.left, rect.top));
                let threshold = drag_threshold(dpi);
                let seen = with_state(|s| {
                    let w = s.windows.get_mut(&key)?;
                    let first = !std::mem::replace(&mut w.hover, true);
                    let changed = std::mem::replace(&mut w.over_button, over) != over;
                    Some((first, changed, w.gesture.moved(at, origin, threshold)))
                })
                .flatten();
                let (first, changed, step) = seen.unwrap_or((false, false, Step::Nothing));
                if first {
                    let mut track = TRACKMOUSEEVENT {
                        cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                        dwFlags: TME_LEAVE,
                        hwndTrack: hwnd,
                        dwHoverTime: 0,
                    };
                    // SAFETY: asks for WM_MOUSELEAVE for this window.
                    unsafe { TrackMouseEvent(&mut track) };
                }
                if first || changed {
                    update_button(hwnd);
                }
                do_step(hwnd, step, lparam);
                0
            }
            WM_MOUSELEAVE => {
                with_state(|s| {
                    if let Some(w) = s.windows.get_mut(&key) {
                        w.hover = false;
                        w.over_button = false;
                    }
                });
                update_button(hwnd);
                0
            }
            WM_LBUTTONDOWN => {
                // SAFETY: the window takes the mouse until the button is released.
                unsafe { SetCapture(hwnd) };
                let rect = rect_of(hwnd);
                let (x, y) = (lo(lparam), hi(lparam));
                let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
                let over = on_more_button(x, y, width, height, dpi_of(hwnd));
                let at = (rect.left + x, rect.top + y);
                let now = Instant::now();
                let step = gesture(hwnd, |g| g.down(over, at, now));
                if matches!(step, Step::Android(_)) {
                    let hold = HOLD_TO_MOVE.as_millis() as u32;
                    // SAFETY: a timer of this window, killed when the press ends.
                    unsafe { SetTimer(hwnd, HOLD_TIMER, hold, None) };
                }
                do_step(hwnd, step, lparam);
                0
            }
            WM_LBUTTONUP => {
                // SAFETY: the hold timer of this window.
                unsafe { KillTimer(hwnd, HOLD_TIMER) };
                let rect = rect_of(hwnd);
                let (width, height) = (rect.right - rect.left, rect.bottom - rect.top);
                let over = on_more_button(lo(lparam), hi(lparam), width, height, dpi_of(hwnd));
                let step = gesture(hwnd, |g| g.up(over));
                // SAFETY: gives the mouse back; the gesture has ended, so losing it ends
                // nothing.
                unsafe { ReleaseCapture() };
                do_step(hwnd, step, lparam);
                0
            }
            WM_CAPTURECHANGED => {
                // SAFETY: the hold timer of this window.
                unsafe { KillTimer(hwnd, HOLD_TIMER) };
                let step = gesture(hwnd, Gesture::cancel);
                do_step(hwnd, step, 0);
                0
            }
            WM_TIMER if wparam == HOLD_TIMER => {
                let mut cursor = POINT { x: 0, y: 0 };
                // SAFETY: the hold timer of this window, and a plain read of the cursor.
                unsafe {
                    KillTimer(hwnd, HOLD_TIMER);
                    GetCursorPos(&mut cursor);
                }
                let rect = rect_of(hwnd);
                let (at, origin) = ((cursor.x, cursor.y), (rect.left, rect.top));
                let now = Instant::now();
                let step = gesture(hwnd, |g| g.held(at, origin, now));
                do_step(hwnd, step, 0);
                0
            }
            WM_TIMER if wparam == FADE_TIMER => {
                present(hwnd);
                let now = Instant::now();
                let done = with_state(|s| s.windows.get(&key).map(|w| w.fade.done(now)));
                if done.flatten().unwrap_or(true) {
                    // SAFETY: the fade timer of this window.
                    unsafe { KillTimer(hwnd, FADE_TIMER) };
                }
                0
            }
            WM_SETFOCUS | WM_KILLFOCUS => {
                let focused = msg == WM_SETFOCUS;
                let view = with_state(|s| {
                    let w = s.windows.get_mut(&key)?;
                    w.focused = focused;
                    Some(Arc::clone(&w.view))
                })
                .flatten();
                update_button(hwnd);
                if let (true, Some(view)) = (focused, view) {
                    widget_automation::raise_focus(&view);
                }
                0
            }
            WM_KEYDOWN if matches!(wparam as u16, VK_RETURN | VK_SPACE) => {
                open_menu(hwnd, None, true);
                0
            }
            WM_WIDGET_MORE => {
                open_menu(hwnd, None, true);
                0
            }
            WM_WIDGET_FOCUS => {
                // SAFETY: a window of this thread takes the keyboard, as UI Automation asked.
                unsafe {
                    SetForegroundWindow(hwnd);
                    SetFocus(hwnd);
                }
                0
            }
            WM_GETOBJECT => {
                let provider = with_state(|s| {
                    let w = s.windows.get_mut(&key)?;
                    let view = Arc::clone(&w.view);
                    let slot = &mut w.provider;
                    let provider = slot.get_or_insert_with(|| widget_automation::provider(&view));
                    Some(provider.clone())
                })
                .flatten();
                match provider.and_then(|p| widget_automation::answer(key, wparam, lparam, &p)) {
                    Some(result) => result,
                    // SAFETY: the default handling of every other object.
                    None => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
                }
            }
            WM_SETTINGCHANGE | WM_THEMECHANGED | WM_SYSCOLORCHANGE => {
                // The button follows the theme and high contrast as they change.
                with_state(|s| s.theme = read_theme());
                // SAFETY: the default handling of the message.
                unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
            }
            WM_CONTEXTMENU => {
                // From the keyboard (the menu key or Shift+F10) the position is -1, -1.
                let at = (lparam as u32 != u32::MAX).then(|| (lo(lparam), hi(lparam)));
                open_menu(hwnd, at, at.is_none());
                0
            }
            WM_CLOSE => 0,
            WM_NCDESTROY => {
                // SAFETY: the window is still alive until this message returns.
                unsafe {
                    crate::app_window::pointer::forget_contacts(hwnd);
                    DefWindowProcW(hwnd, msg, wparam, lparam)
                }
            }
            m if m != 0 && with_state(|s| s.taskbar_created).unwrap_or(0) == m => {
                // The shell restarted: the window belongs to its new desktop.
                // SAFETY: a window of this thread.
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_HWNDPARENT, desktop_owner() as isize);
                    SetWindowPos(
                        hwnd,
                        HWND_BOTTOM,
                        0,
                        0,
                        0,
                        0,
                        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOSIZE,
                    );
                }
                0
            }
            // SAFETY: the default handling of every other message.
            _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
        }
    }

    /// The menu of the window is open (or closed again): the More button shows while it is.
    /// Once it closes the pointer may be elsewhere, so the window looks again.
    fn set_menu_open(hwnd: HWND, open: bool) {
        let key = hwnd as isize;
        let rect = rect_of(hwnd);
        let mut cursor = POINT { x: 0, y: 0 };
        // SAFETY: a plain read of the cursor's position.
        unsafe { GetCursorPos(&mut cursor) };
        let inside = cursor.x >= rect.left
            && cursor.y >= rect.top
            && cursor.x < rect.right
            && cursor.y < rect.bottom;
        with_state(|s| {
            if let Some(w) = s.windows.get_mut(&key) {
                w.menu_open = open;
                if !open {
                    w.hover = inside;
                    w.over_button = false;
                }
            }
        });
        if !open && inside {
            let mut track = TRACKMOUSEEVENT {
                cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            // SAFETY: asks for WM_MOUSELEAVE for this window again.
            unsafe { TrackMouseEvent(&mut track) };
        }
        update_button(hwnd);
    }

    /// Shows the window's menu (Resize, Remove) at `at` on the screen, where the user
    /// right-clicked, or under the More button (None), and does what the user chose.
    /// `keyboard` says the keyboard opened it.
    fn open_menu(hwnd: HWND, at: Option<(i32, i32)>, keyboard: bool) {
        let key = hwnd as isize;
        let Some((provider, placement, widget_id)) = with_state(|s| {
            s.windows.get(&key).map(|w| {
                (
                    w.widget.provider.clone(),
                    w.widget.placement,
                    w.widget.widget_id,
                )
            })
        })
        .flatten() else {
            return;
        };
        let rect = rect_of(hwnd);
        let anchor = match at {
            Some((x, y)) => Anchor::Point { x, y },
            None => Anchor::Below(button_on_screen(hwnd)),
        };
        let dpi = dpi_of(hwnd);
        // The size shown as current is the window's own, which a drag may have changed.
        let shown = placement_of(
            rect.left,
            rect.top,
            rect.right - rect.left,
            rect.bottom - rect.top,
            dpi,
        );
        let placement = Placement {
            width_dp: shown.width_dp,
            height_dp: shown.height_dp,
            ..placement
        };
        set_menu_open(hwnd, true);
        let choice = widget_menu::show(key, anchor, keyboard, &provider, &placement);
        set_menu_open(hwnd, false);
        match choice {
            Some(MenuChoice::Resize {
                width_dp,
                height_dp,
            }) => {
                let (width, height) = window_size(
                    &Placement {
                        width_dp,
                        height_dp,
                        ..placement
                    },
                    dpi,
                );
                // SAFETY: a window of this thread; it keeps its place and its z-order.
                unsafe {
                    SetWindowPos(
                        hwnd,
                        null_mut(),
                        0,
                        0,
                        width,
                        height,
                        SWP_NOACTIVATE | SWP_NOMOVE | SWP_NOZORDER,
                    );
                }
                report_placement(hwnd);
            }
            Some(MenuChoice::Remove) => {
                with_state(|s| {
                    let _ = s.reports.send(WindowReport::Remove { widget_id });
                });
            }
            None => {}
        }
    }

    /// Reports pointer input inside the widget, in its frame's pixels. Moves count only while
    /// the button is down.
    fn pointer(hwnd: HWND, action: PointerAction, lparam: LPARAM) {
        let rect = rect_of(hwnd);
        let window = (rect.right - rect.left, rect.bottom - rect.top);
        let (x, y) = (lo(lparam), hi(lparam));
        with_state(|s| {
            let Some(w) = s.windows.get(&(hwnd as isize)) else {
                return;
            };
            if action == PointerAction::Move && !w.pressed {
                return;
            }
            let frame = w
                .frame
                .as_ref()
                .map(|f| (f.width, f.height))
                .unwrap_or((window.0.max(1) as u32, window.1.max(1) as u32));
            let (fx, fy) = to_frame(x, y, window, frame);
            let _ = s.reports.send(WindowReport::Pointer {
                widget_id: w.widget.widget_id,
                action,
                x: fx,
                y: fy,
            });
        });
    }
}

/// Decodes widget frames, and the icons of the apps that run in the background, with the
/// Windows Imaging Component.
#[cfg(windows)]
pub(crate) mod png {
    use windows::core::GUID;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_WICPixelFormat32bppBGRA, GUID_WICPixelFormat32bppPBGRA,
        IWICBitmapSource, IWICImagingFactory, IWICPalette, WICBitmapDitherTypeNone,
        WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom, WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::{CoCreateInstance, CLSCTX_INPROC_SERVER};

    pub(crate) struct Decoder {
        factory: IWICImagingFactory,
    }

    /// Runs `f` with COM entered on this thread, as the imaging component needs, and leaves
    /// it as it was. A thread already in COM, in either apartment, stays in it.
    pub(crate) fn with_com<T>(f: impl FnOnce() -> T) -> T {
        use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};
        // SAFETY: enters COM for this thread; undone below only when this call entered it.
        let entered = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) }.is_ok();
        let result = f();
        if entered {
            // SAFETY: balances the successful CoInitializeEx above.
            unsafe { CoUninitialize() };
        }
        result
    }

    /// The width and height a PNG says it has, from its header chunk.
    pub(crate) fn size(png: &[u8]) -> Option<(u32, u32)> {
        const SIGNATURE: &[u8] = b"\x89PNG\r\n\x1a\n";
        if png.len() < 24 || &png[..8] != SIGNATURE || &png[12..16] != b"IHDR" {
            return None;
        }
        let width = u32::from_be_bytes(png[16..20].try_into().ok()?);
        let height = u32::from_be_bytes(png[20..24].try_into().ok()?);
        (width > 0 && height > 0).then_some((width, height))
    }

    impl Decoder {
        pub(crate) fn new() -> Result<Decoder, String> {
            // SAFETY: COM is entered on this thread.
            let factory: IWICImagingFactory =
                unsafe { CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER) }
                    .map_err(|e| format!("the imaging component is not there: {e}"))?;
            Ok(Decoder { factory })
        }

        /// The PNG image `png`, scaled to `width` by `height`, as premultiplied BGRA rows.
        pub(crate) fn decode(
            &self,
            png: &[u8],
            width: u32,
            height: u32,
        ) -> Result<Vec<u8>, String> {
            self.decode_as(png, width, height, &GUID_WICPixelFormat32bppPBGRA)
        }

        /// The same with straight alpha, as an icon's colour bitmap takes it.
        pub(crate) fn decode_straight(
            &self,
            png: &[u8],
            width: u32,
            height: u32,
        ) -> Result<Vec<u8>, String> {
            self.decode_as(png, width, height, &GUID_WICPixelFormat32bppBGRA)
        }

        fn decode_as(
            &self,
            png: &[u8],
            width: u32,
            height: u32,
            format: &GUID,
        ) -> Result<Vec<u8>, String> {
            let fail = |what: &str, e: windows::core::Error| format!("{what}: {e}");
            // SAFETY: COM calls on interfaces this thread made, with buffers that outlive them.
            unsafe {
                let stream = self
                    .factory
                    .CreateStream()
                    .map_err(|e| fail("reading the image", e))?;
                stream
                    .InitializeFromMemory(png)
                    .map_err(|e| fail("reading the image", e))?;
                let decoder = self
                    .factory
                    .CreateDecoderFromStream(
                        &stream,
                        std::ptr::null(),
                        WICDecodeMetadataCacheOnDemand,
                    )
                    .map_err(|e| fail("the image is not a PNG", e))?;
                let frame = decoder
                    .GetFrame(0)
                    .map_err(|e| fail("the image has no frame", e))?;
                let converter = self
                    .factory
                    .CreateFormatConverter()
                    .map_err(|e| fail("converting the image", e))?;
                converter
                    .Initialize(
                        &frame,
                        format,
                        WICBitmapDitherTypeNone,
                        None::<&IWICPalette>,
                        0.0,
                        WICBitmapPaletteTypeCustom,
                    )
                    .map_err(|e| fail("converting the image", e))?;
                let scaler = self
                    .factory
                    .CreateBitmapScaler()
                    .map_err(|e| fail("scaling the image", e))?;
                scaler
                    .Initialize(&converter, width, height, WICBitmapInterpolationModeFant)
                    .map_err(|e| fail("scaling the image", e))?;
                let source: IWICBitmapSource = scaler.into();
                let stride = width * 4;
                let mut pixels = vec![0u8; (stride * height) as usize];
                source
                    .CopyPixels(std::ptr::null(), stride, &mut pixels)
                    .map_err(|e| fail("copying the image", e))?;
                Ok(pixels)
            }
        }
    }
}

/// Whether the window manager starts Android at sign-in, seeing it in `state`: when it is not
/// running. One that is starting, running or dozing is left as it is.
pub fn starts_android_at_sign_in(state: SessionState) -> bool {
    state == SessionState::Stopped
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr10_the_window_manager_starts_a_stopped_android_at_sign_in() {
        assert!(starts_android_at_sign_in(SessionState::Stopped));
        for running in [
            SessionState::Starting,
            SessionState::Active,
            SessionState::Dozing,
            SessionState::Frozen,
        ] {
            assert!(!starts_android_at_sign_in(running), "{running:?}");
        }
    }
    use dsa_host_core::widgets::WidgetProvider;

    fn clock() -> WidgetProvider {
        WidgetProvider {
            provider: "com.example.clock/.ClockWidget".into(),
            package_name: "com.example.clock".into(),
            label: "Clock".into(),
            preview_png: Vec::new(),
            min_width_dp: 110,
            min_height_dp: 40,
            default_width_dp: 180,
            default_height_dp: 110,
            max_width_dp: 0,
            max_height_dp: 0,
            resize_mode: 3,
            needs_configuration: false,
            app_label: "Clock App".into(),
        }
    }

    #[test]
    fn fr12_a_widget_window_without_an_image_shows_a_visible_placeholder() {
        let (width, height) = (600usize, 340usize);
        let pixels = placeholder(width, height, 192);
        assert_eq!(pixels.len(), width * height * 4);
        assert!(!all_clear(&pixels));
        let alpha = |x: usize, y: usize| pixels[(y * width + x) * 4 + 3];
        // The middle and every edge's midpoint are mostly opaque, so the window is never a
        // see-through hole on the desktop.
        for (x, y) in [
            (width / 2, height / 2),
            (0, height / 2),
            (width - 1, height / 2),
            (width / 2, 0),
            (width / 2, height - 1),
        ] {
            assert!(alpha(x, y) >= 0xc0, "alpha at {x},{y} is {}", alpha(x, y));
        }
        // The corners are rounded off.
        assert_eq!(alpha(0, 0), 0);
        assert_eq!(alpha(width - 1, height - 1), 0);
        // Premultiplied: no colour exceeds its alpha.
        assert!(pixels
            .chunks_exact(4)
            .all(|p| p[..3].iter().all(|&c| c <= p[3])));
        // A tiny window still gets one.
        assert!(!all_clear(&placeholder(3, 2, 96)));
    }

    #[test]
    fn fr12_an_image_with_nothing_in_it_is_told_from_one_with_something_drawn() {
        assert!(all_clear(&[0, 0, 0, 0, 9, 9, 9, 0]));
        assert!(!all_clear(&[0, 0, 0, 0, 0, 0, 0, 1]));
    }

    #[test]
    fn fr12_a_widget_windows_size_follows_its_monitors_scale() {
        let placement = Placement {
            x: 10,
            y: 20,
            width_dp: 180,
            height_dp: 110,
        };
        assert_eq!(window_size(&placement, 96), (180, 110));
        assert_eq!(window_size(&placement, 144), (270, 165));
        assert_eq!(placement_of(10, 20, 270, 165, 144), placement);
        assert_eq!(android_density(96), 160);
        assert_eq!(android_density(144), 240);
        assert_eq!(android_density(120), 200);
        assert_eq!(dip_of_px(361, 240), 144);
        assert_eq!(min_window_size(&clock(), 192), (220, 80));
    }

    #[test]
    fn fr12_a_widget_window_is_kept_off_the_taskbar_and_alt_tab_and_never_takes_the_focus() {
        let (style, ex) = window_styles();
        assert_eq!(style, 0x8000_0000, "a popup with no frame");
        assert_ne!(ex & 0x80, 0, "a tool window: not in the taskbar or Alt+Tab");
        assert_ne!(
            ex & 0x0008_0000,
            0,
            "layered: see-through where the widget is"
        );
        assert_ne!(ex & 0x0800_0000, 0, "never takes the focus");
        assert_eq!(ex & 0x0000_0008, 0, "never topmost");
    }

    #[test]
    fn fr12_a_widget_window_is_resized_by_its_edges_and_has_no_band_along_its_top() {
        let (w, h) = (200, 120);
        assert_eq!(hit_test(100, 60, w, h, 96, 3), Hit::Client);
        assert_eq!(hit_test(100, 10, w, h, 96, 3), Hit::Client, "no band");
        assert_eq!(hit_test(2, 60, w, h, 96, 3), Hit::Left);
        assert_eq!(hit_test(198, 60, w, h, 96, 3), Hit::Right);
        assert_eq!(hit_test(100, 118, w, h, 96, 3), Hit::Bottom);
        assert_eq!(hit_test(100, 2, w, h, 96, 3), Hit::Top);
        assert_eq!(hit_test(198, 118, w, h, 96, 3), Hit::BottomRight);
        assert_eq!(hit_test(1, 1, w, h, 96, 3), Hit::TopLeft);
        assert_eq!(hit_test(198, 2, w, h, 96, 3), Hit::TopRight);
        // Only the edges the widget may be resized along resize it.
        assert_eq!(hit_test(2, 60, w, h, 96, 2), Hit::Client);
        assert_eq!(hit_test(100, 118, w, h, 96, 1), Hit::Client);
        assert_eq!(hit_test(100, 2, w, h, 96, 0), Hit::Client);
        // The edges grow with the scale.
        assert_eq!(hit_test(100, 30, w, h, 192, 3), Hit::Client);
        assert_eq!(hit_test(10, 60, w, h, 192, 3), Hit::Left);
    }

    #[test]
    fn fr12_the_more_button_is_a_28_px_circle_in_the_top_right_corner() {
        // At 100%: 28 px across, 8 px from the top and right edges.
        let at_100 = more_button(200, 120, 96);
        assert_eq!((at_100.cx, at_100.cy, at_100.radius), (178.0, 22.0, 14.0));
        assert_eq!(at_100.bounds(), (164, 8, 192, 36));
        // It grows with the scale.
        let at_150 = more_button(300, 180, 144);
        assert_eq!((at_150.cx, at_150.cy, at_150.radius), (267.0, 33.0, 21.0));
        let at_200 = more_button(400, 240, 192);
        assert_eq!(at_200.radius, 28.0);
        // A widget too small for it gets one that still fits.
        let tiny = more_button(30, 24, 96);
        assert!(tiny.radius * 2.0 <= 22.0);
        assert!(tiny.cx + tiny.radius <= 30.0 && tiny.cy - tiny.radius >= 0.0);
        // It stays off the edges that resize the window.
        let border = BORDER_DIP as f32;
        assert!(at_100.cx + at_100.radius <= 200.0 - border);
        assert!(at_100.cy - at_100.radius >= border);
    }

    #[test]
    fn fr12_the_more_button_is_hit_inside_its_circle_only() {
        let (w, h) = (200, 120);
        assert_eq!(hit_test(178, 22, w, h, 96, 3), Hit::More);
        assert!(on_more_button(178, 22, w, h, 96));
        assert!(on_more_button(166, 22, w, h, 96), "the left of the circle");
        assert!(on_more_button(178, 34, w, h, 96), "its bottom");
        // The corners of its square are the widget's.
        assert!(!on_more_button(165, 9, w, h, 96));
        assert_eq!(hit_test(165, 9, w, h, 96, 3), Hit::Client);
        assert!(!on_more_button(191, 35, w, h, 96));
        // Outside it, everywhere is the widget's.
        assert!(!on_more_button(150, 22, w, h, 96));
        assert!(!on_more_button(178, 40, w, h, 96));
        // At 200% it is twice as large.
        assert!(on_more_button(2 * 166, 2 * 22, 2 * w, 2 * h, 192));
    }

    #[test]
    fn fr12_the_more_button_stays_inside_the_widgets_rounded_corners() {
        // Android draws widgets with corners of 16 to 28 dp; the placeholder's are 12. Every
        // radius up to 40 dp keeps the circle inside, at every scale.
        for dpi in [96, 120, 144, 192] {
            let (width, height) = window_size(
                &Placement {
                    x: 0,
                    y: 0,
                    width_dp: 180,
                    height_dp: 110,
                },
                dpi,
            );
            let circle = more_button(width, height, dpi);
            for radius_dp in 0..=40 {
                let radius = px_of_dip(radius_dp, dpi) as f32;
                assert!(
                    inside_rounded_window(&circle, width, height, radius),
                    "at {dpi} dpi the button leaves corners of {radius_dp} dp"
                );
            }
        }
        // The check itself tells a circle that leaves a corner.
        let corner = Circle {
            cx: 190.0,
            cy: 10.0,
            radius: 9.0,
        };
        assert!(!inside_rounded_window(&corner, 200, 120, 30.0));
        assert!(inside_rounded_window(&corner, 200, 120, 0.0));
    }

    #[test]
    fn fr12_the_more_button_fades_in_on_hover_and_keyboard_focus_and_out_after() {
        let start = Instant::now();
        let idle = Gesture::Idle;
        assert!(!more_button_shown(false, false, false, &idle));
        // The pointer is over the widget, the keyboard is on it, its menu is open.
        assert!(more_button_shown(true, false, false, &idle));
        assert!(more_button_shown(false, true, false, &idle));
        assert!(more_button_shown(false, false, true, &idle));
        let moving = Gesture::Moving { grab: (4, 4) };
        assert!(more_button_shown(false, false, false, &moving));
        // Fading in takes WinUI's fast animation, and turning back starts from where it is.
        let hidden = Fade::hidden(start);
        assert_eq!(hidden.level(start + FADE), 0.0);
        let fading_in = hidden.toward(true, start);
        assert_eq!(fading_in.level(start), 0.0);
        let half = fading_in.level(start + FADE / 2);
        assert!(half > 0.4 && half < 0.6, "halfway: {half}");
        assert_eq!(fading_in.level(start + FADE), 1.0);
        assert!(fading_in.done(start + FADE));
        assert!(!fading_in.done(start + FADE / 2));
        assert_eq!(fading_in.toward(true, start + FADE / 2), fading_in);
        let back = fading_in.toward(false, start + FADE / 2);
        assert!((back.level(start + FADE / 2) - half).abs() < 0.01);
        assert_eq!(back.level(start + FADE / 2 + FADE), 0.0);
    }

    #[test]
    fn fr12_the_more_button_is_drawn_with_a_subtle_fill_a_hairline_and_its_states() {
        use crate::compositor::fluent::{button_palette, Theme};
        let (width, height) = (200usize, 120usize);
        let base = placeholder(width, height, 96);
        let palette = button_palette(&Theme::light());
        let draw = |level: f32, hover: bool, pressed: bool| {
            let mut pixels = base.clone();
            let look = ButtonLook {
                level,
                hover,
                pressed,
                palette,
                glyph: None,
            };
            draw_more_button(&mut pixels, width, height, 96, &look);
            pixels
        };
        let at = |pixels: &[u8], x: usize, y: usize| {
            let index = (y * width + x) * 4;
            [
                pixels[index],
                pixels[index + 1],
                pixels[index + 2],
                pixels[index + 3],
            ]
        };
        // Not shown: the widget is untouched.
        assert_eq!(draw(0.0, false, false), base);
        let rest = draw(1.0, false, false);
        let hover = draw(1.0, true, false);
        let pressed = draw(1.0, false, true);
        // The circle's middle takes the light fill over the dark placeholder.
        assert!(at(&rest, 178, 22)[0] > at(&base, 178, 22)[0] + 0x60);
        assert_ne!(at(&hover, 178, 22), at(&rest, 178, 22));
        assert_ne!(at(&pressed, 178, 22), at(&hover, 178, 22));
        // The hairline sits on the circle's edge, and the square's corner stays the widget's.
        assert_ne!(at(&rest, 178, 8), at(&rest, 178, 12));
        assert_eq!(at(&rest, 165, 9), at(&base, 165, 9));
        // Elsewhere the widget is untouched.
        assert_eq!(at(&rest, 100, 60), at(&base, 100, 60));
        assert_eq!(at(&rest, 150, 22), at(&base, 150, 22));
        // Half faded in, it is between the widget and the button.
        let half = draw(0.5, false, false);
        let blue = |pixels: &[u8]| at(pixels, 178, 22)[0];
        let (b, h, r) = (blue(&base), blue(&half), blue(&rest));
        assert!(h > b && h < r, "{b} < {h} < {r}");
        // Premultiplied: no color exceeds its alpha.
        assert!(rest
            .chunks_exact(4)
            .all(|p| p[..3].iter().all(|&c| c <= p[3])));
    }

    fn pressed_on_button(down: (i32, i32)) -> Gesture {
        let mut gesture = Gesture::Idle;
        gesture.down(true, down, Instant::now());
        gesture
    }

    #[test]
    fn fr12_a_tap_and_a_drag_on_the_widget_stay_the_widgets() {
        let start = Instant::now();
        let (origin, threshold) = ((100, 100), (4, 4));
        let mut gesture = Gesture::Idle;
        assert_eq!(
            gesture.down(false, (150, 150), start),
            Step::Android(PointerAction::Down)
        );
        // A tap: the press and the release go to Android.
        assert_eq!(gesture.up(false), Step::Android(PointerAction::Up));
        assert_eq!(gesture, Gesture::Idle);
        // A drag that starts at once (a list scrolling) is the widget's, however long it lasts.
        gesture.down(false, (150, 150), start);
        let moved = gesture.moved((150, 170), origin, threshold);
        assert_eq!(moved, Step::Android(PointerAction::Move));
        let later = start + HOLD_TO_MOVE * 2;
        assert_eq!(gesture.held((150, 170), origin, later), Step::Nothing);
        assert_eq!(gesture.up(false), Step::Android(PointerAction::Up));
        // The window losing the mouse cancels Android's press.
        gesture.down(false, (150, 150), start);
        assert_eq!(gesture.cancel(), Step::Android(PointerAction::Cancel));
    }

    #[test]
    fn fr12_holding_a_press_still_then_dragging_moves_the_widget() {
        let start = Instant::now();
        let (origin, threshold) = ((100, 100), (4, 4));
        let mut gesture = Gesture::Idle;
        gesture.down(false, (150, 150), start);
        // A small wobble within the threshold is still a press.
        gesture.moved((152, 151), origin, threshold);
        let early = start + HOLD_TO_MOVE / 2;
        assert_eq!(gesture.held((152, 151), origin, early), Step::Nothing);
        // Held still long enough: Android's press is cancelled and the window starts moving.
        let held = gesture.held((152, 151), origin, start + HOLD_TO_MOVE);
        let start_move = Step::StartMove {
            cancel: true,
            to: origin,
        };
        assert_eq!(held, start_move);
        // The window follows the pointer, keeping where it was grabbed.
        let moved = gesture.moved((252, 201), origin, threshold);
        assert_eq!(moved, Step::MoveTo((200, 150)));
        assert_eq!(gesture.up(false), Step::EndMove);
        assert_eq!(gesture, Gesture::Idle);
    }

    #[test]
    fn fr12_the_more_button_opens_the_menu_on_a_click_and_moves_the_widget_on_a_drag() {
        let (origin, threshold) = ((100, 100), (4, 4));
        // A click on the button opens the menu; nothing reaches Android.
        let mut gesture = pressed_on_button((278, 122));
        assert_eq!(gesture.moved((279, 123), origin, threshold), Step::Nothing);
        assert_eq!(gesture.up(true), Step::OpenMenu);
        // A press that leaves the button before it is let go does nothing.
        let mut gesture = pressed_on_button((278, 122));
        assert_eq!(gesture.up(false), Step::Button);
        // Dragging the button moves the window at once, by as far as the pointer went.
        let mut gesture = pressed_on_button((278, 122));
        let start = gesture.moved((288, 122), origin, threshold);
        let start_move = Step::StartMove {
            cancel: false,
            to: (110, 100),
        };
        assert_eq!(start, start_move);
        assert_eq!(
            gesture.moved((300, 140), origin, threshold),
            Step::MoveTo((122, 118))
        );
        // A drag never opens the menu.
        assert_eq!(gesture.up(true), Step::EndMove);
        let mut gesture = pressed_on_button((278, 122));
        gesture.moved((300, 122), origin, threshold);
        assert_eq!(gesture.cancel(), Step::EndMove);
    }

    /// Pictures of the widget menu (its first level and its Resize submenu) and of the More
    /// button at rest, under the pointer and pressed, in the light and the dark theme at 100%
    /// and 150%, kept with the Test run when `DSA_WIDGET_MENU_PICTURES` names a folder.
    #[cfg(windows)]
    #[test]
    fn fr12_pictures_of_the_widget_menu_and_its_more_button() {
        use crate::compositor::fluent::text::{self, Align, ICON_FACES, TEXT_FACES};
        use crate::compositor::fluent::{
            button_palette, glyph, menu_palette, premultiplied, Theme,
        };
        use crate::compositor::fluent_menu::{self, metrics, Look, Mask, MenuEntry};
        use crate::compositor::widget_menu;

        let folder = std::env::var_os("DSA_WIDGET_MENU_PICTURES").map(std::path::PathBuf::from);
        let desktop = [0xA5, 0x6E, 0x3A];
        let sizes = widget_menu::size_choices(&clock());
        let placement = Placement {
            x: 0,
            y: 0,
            width_dp: 180,
            height_dp: 110,
        };
        for dark in [false, true] {
            // Transparency off: the solid fill DWM's acrylic falls back to, as a picture can
            // show it.
            let theme = Theme {
                dark,
                transparency: false,
                ..Theme::light()
            };
            let palette = menu_palette(&theme);
            for dpi in [96, 144] {
                let words = widget_menu::menu_text(false);
                let entries = widget_menu::entries(&words, &sizes, &placement);
                let submenu = match entries[0].item() {
                    Some(item) => item.submenu.clone(),
                    None => Vec::new(),
                };
                let draw = |entries: &[MenuEntry], focus: Option<usize>| {
                    let em = px_of_dip(metrics::TEXT_SIZE, dpi);
                    let widths: Vec<i32> = entries
                        .iter()
                        .map(|entry| match entry.item() {
                            Some(item) => text::measure(&item.label, &TEXT_FACES, em),
                            None => 0,
                        })
                        .collect();
                    let laid = fluent_menu::layout(entries, &widths, dpi);
                    let art = fluent_menu::row_art(entries, &laid);
                    let look = Look {
                        layout: &laid,
                        art: &art,
                        entries,
                        focus,
                        pressed: None,
                        cues: true,
                        palette: &palette,
                    };
                    let pixels = fluent_menu::render(&look);
                    (laid, pixels)
                };
                let (root, root_pixels) = draw(&entries, Some(0));
                let (sub, sub_pixels) = draw(&submenu, Some(2));
                // The labels are drawn: the first row's label box is not all background.
                let label = root.rows[0].text;
                let background = premultiplied(palette.background, 1.0);
                let inked = (label.y..label.y + label.h)
                    .flat_map(|y| (label.x..label.x + label.w).map(move |x| (x, y)))
                    .filter(|&(x, y)| {
                        let index = ((y * root.width + x) * 4) as usize;
                        root_pixels[index..index + 4] != background
                    })
                    .count();
                assert!(
                    inked > 20,
                    "the label {:?} has {inked} inked pixels",
                    words.resize
                );
                let mut picture = Picture::new(
                    (root.width + sub.width) as usize + 48,
                    root.height.max(sub.height) as usize + 32,
                    desktop,
                );
                let (root_w, root_h) = (root.width as usize, root.height as usize);
                picture.place(16, 16, root_w, root_h, &root_pixels);
                let (sub_w, sub_h) = (sub.width as usize, sub.height as usize);
                picture.place(root_w + 32, 16, sub_w, sub_h, &sub_pixels);
                let percent = dpi * 100 / 96;
                let name = if dark { "dark" } else { "light" };
                picture.save(folder.as_deref(), &format!("menu-{name}-{percent}.bmp"));

                // The More button over the placeholder, at rest, hovered and pressed.
                let (width, height) = window_size(&placement, dpi);
                let size = more_glyph_size(width, height, dpi);
                let em = px_of_dip(MORE_GLYPH_DIP, dpi).min(size as i32);
                let symbol = glyph::MORE.to_string();
                let cover = text::mask(&symbol, &ICON_FACES, em, (size, size), Align::Center);
                let mask = cover.map(|cover| Mask {
                    width: size,
                    height: size,
                    cover,
                });
                let (w, h) = (width as usize, height as usize);
                let mut picture = Picture::new(3 * w + 64, h + 32, desktop);
                let states = [(false, false), (true, false), (false, true)];
                for (index, (hover, pressed)) in states.into_iter().enumerate() {
                    let mut pixels = placeholder(w, h, dpi);
                    let look = ButtonLook {
                        level: 1.0,
                        hover,
                        pressed,
                        palette: button_palette(&theme),
                        glyph: mask.as_ref(),
                    };
                    draw_more_button(&mut pixels, w, h, dpi, &look);
                    picture.place(16 + index * (w + 16), 16, w, h, &pixels);
                }
                picture.save(folder.as_deref(), &format!("button-{name}-{percent}.bmp"));
            }
        }
    }

    /// An opaque picture of `width` by `height` pixels on a desktop color, saved as a BMP.
    #[cfg(windows)]
    struct Picture {
        width: usize,
        height: usize,
        /// Opaque BGRA rows, top first.
        pixels: Vec<u8>,
    }

    #[cfg(windows)]
    impl Picture {
        fn new(width: usize, height: usize, color: [u8; 3]) -> Picture {
            let pixels = [color[0], color[1], color[2], 0xff].repeat(width * height);
            Picture {
                width,
                height,
                pixels,
            }
        }

        /// Lays premultiplied BGRA `source` of `w` by `h` over the picture at `x`, `y`.
        fn place(&mut self, x: usize, y: usize, w: usize, h: usize, source: &[u8]) {
            for row in 0..h.min(self.height.saturating_sub(y)) {
                for column in 0..w.min(self.width.saturating_sub(x)) {
                    let from = (row * w + column) * 4;
                    let to = ((y + row) * self.width + x + column) * 4;
                    let keep = 255 - u32::from(source[from + 3]);
                    for channel in 0..3 {
                        let under = u32::from(self.pixels[to + channel]) * keep / 255;
                        let over = u32::from(source[from + channel]);
                        self.pixels[to + channel] = (over + under).min(255) as u8;
                    }
                }
            }
        }

        /// Writes the picture as a 32-bit BMP named `name` into `folder`, when there is one.
        fn save(&self, folder: Option<&std::path::Path>, name: &str) {
            let Some(folder) = folder else {
                return;
            };
            std::fs::create_dir_all(folder).unwrap();
            let mut out = Vec::with_capacity(54 + self.pixels.len());
            let size = (54 + self.pixels.len()) as u32;
            out.extend_from_slice(b"BM");
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&[0, 0, 0, 0]);
            out.extend_from_slice(&54u32.to_le_bytes());
            out.extend_from_slice(&40u32.to_le_bytes());
            out.extend_from_slice(&(self.width as i32).to_le_bytes());
            // Negative: the rows come top first.
            out.extend_from_slice(&(-(self.height as i32)).to_le_bytes());
            out.extend_from_slice(&1u16.to_le_bytes());
            out.extend_from_slice(&32u16.to_le_bytes());
            out.extend_from_slice(&[0; 24]);
            out.extend_from_slice(&self.pixels);
            std::fs::write(folder.join(name), out).unwrap();
        }
    }

    #[test]
    fn fr12_a_widget_window_off_every_monitor_comes_back_into_view() {
        let areas = [(0, 0, 1920, 1040)];
        assert_eq!(reachable_origin(100, 100, 200, 100, &areas), (100, 100));
        assert_eq!(reachable_origin(-150, 100, 200, 100, &areas), (-150, 100));
        assert_eq!(reachable_origin(3000, 100, 200, 100, &areas), (1720, 100));
        assert_eq!(reachable_origin(100, -500, 200, 100, &areas), (100, 0));
        assert_eq!(reachable_origin(5, 5, 10, 10, &[]), (5, 5));
    }

    #[test]
    fn fr12_a_widget_window_sits_right_above_the_desktop_not_under_the_wallpaper() {
        // The bottom of the z-order is under the shell's wallpaper window once it has one.
        assert_eq!(z_place(Some(10), Some(20), 30), ZPlace::After(20));
        assert_eq!(z_place(Some(10), Some(30), 30), ZPlace::Keep);
        assert_eq!(z_place(Some(10), None, 30), ZPlace::Keep);
        assert_eq!(z_place(None, None, 30), ZPlace::Bottom);
    }

    #[test]
    fn fr12_a_widget_window_without_an_image_is_logged_once_after_a_while() {
        let start = Instant::now();
        assert!(!frame_overdue(Some(start), false, start));
        assert!(!frame_overdue(
            Some(start),
            false,
            start + NO_FRAME - Duration::from_millis(1)
        ));
        assert!(frame_overdue(Some(start), false, start + NO_FRAME));
        assert!(!frame_overdue(Some(start), true, start + NO_FRAME * 3));
        assert!(!frame_overdue(None, false, start + NO_FRAME * 3));
    }

    #[test]
    fn fr12_a_point_in_the_window_is_scaled_to_the_widgets_frame() {
        assert_eq!(to_frame(50, 25, (100, 50), (200, 100)), (100.0, 50.0));
        assert_eq!(to_frame(500, -3, (100, 50), (200, 100)), (200.0, 0.0));
        assert_eq!(to_frame(7, 8, (0, 0), (200, 100)), (7.0, 8.0));
    }

    fn pointer(action: PointerAction, x: f32) -> WindowReport {
        WindowReport::Pointer {
            widget_id: 1,
            action,
            x,
            y: 0.0,
        }
    }

    #[test]
    fn fr12_moves_waiting_to_be_sent_give_way_to_the_newest() {
        let reports = vec![
            pointer(PointerAction::Down, 1.0),
            pointer(PointerAction::Move, 2.0),
            pointer(PointerAction::Move, 3.0),
            pointer(PointerAction::Move, 4.0),
            pointer(PointerAction::Up, 4.0),
        ];
        assert_eq!(
            coalesce(reports),
            vec![
                pointer(PointerAction::Down, 1.0),
                pointer(PointerAction::Move, 4.0),
                pointer(PointerAction::Up, 4.0),
            ]
        );
    }

    #[test]
    fn fr12_each_change_to_the_widgets_is_one_command_for_the_windows() {
        let widget = Widget {
            widget_id: 3,
            provider: clock(),
            placement: Placement::default(),
        };
        assert_eq!(
            command_of(WidgetEvent::Placed(widget.clone())),
            Some(WindowCommand::Place(widget))
        );
        assert_eq!(
            command_of(WidgetEvent::Removed { widget_id: 3 }),
            Some(WindowCommand::Remove(3))
        );
        assert_eq!(command_of(WidgetEvent::Listed(vec![3])), None);
    }

    #[test]
    fn fr12_a_widget_window_the_shell_put_under_the_desktop_is_put_back_above_it() {
        let (desktop, own, widget) = (10, 1, 2);
        let fits = |list: &[(isize, bool)], ours: &[isize]| {
            let below: Vec<Below> = list
                .iter()
                .map(|&(hwnd, covers)| Below { hwnd, covers })
                .collect();
            right_above_desktop(&below, desktop, ours)
        };
        // Right above the desktop, with a hidden window and another widget between.
        assert!(fits(
            &[(30, false), (widget, true), (desktop, true)],
            &[own, widget]
        ));
        // The shell raised its desktop window over it: the desktop is not below it.
        assert!(!fits(&[(40, false), (41, false)], &[own]));
        // An app's window is under it, which it would hide.
        assert!(!fits(&[(50, true), (desktop, true)], &[own]));
    }

    /// A placed widget window shows its placeholder from the moment it opens, through the real
    /// windows: it is given its picture before it is shown, the system takes the picture, the
    /// picture is mostly opaque, and the window is visible, on a monitor and right above the
    /// desktop.
    #[cfg(windows)]
    #[test]
    fn fr12_a_widget_window_shows_a_placeholder_then_a_see_through_image_it_keeps() {
        use windows_sys::Win32::Foundation::{HWND, RECT};
        use windows_sys::Win32::Graphics::Gdi::{MonitorFromRect, MONITOR_DEFAULTTONULL};
        use windows_sys::Win32::UI::WindowsAndMessaging::{
            GetWindow, GetWindowLongPtrW, GetWindowRect, IsWindowVisible, GWL_EXSTYLE, GW_HWNDNEXT,
            WS_EX_LAYERED,
        };

        let mut windows = WindowsWidgetWindows::open(None).expect("opening the widget windows");
        let placement = Placement {
            x: 40,
            y: 40,
            width_dp: 180,
            height_dp: 110,
        };
        windows.command(WindowCommand::Place(Widget {
            widget_id: 4242,
            provider: clock(),
            placement,
        }));
        let deadline = Instant::now() + Duration::from_secs(10);
        // Only this test opens widget windows, so the first drawing is its window's.
        let (first, hwnd) = loop {
            let found = win::PRESENTED.lock().unwrap().first().copied();
            if let Some(found) = found {
                break (found, found.hwnd as HWND);
            }
            assert!(
                Instant::now() < deadline,
                "the widget's window was never drawn"
            );
            std::thread::sleep(Duration::from_millis(20));
        };
        // Shown right after its first drawing.
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            first.committed,
            "the system did not take the window's picture"
        );
        assert!(
            !first.visible,
            "the window was shown before it had a picture"
        );
        let pixels = (first.width * first.height) as usize;
        assert!(pixels > 0, "the window is {}x{}", first.width, first.height);
        assert!(
            first.opaque * 10 >= pixels * 9,
            "only {} of {pixels} pixels of the placeholder are opaque",
            first.opaque
        );
        // SAFETY: reads a window of the windows' thread.
        unsafe {
            assert!(IsWindowVisible(hwnd) != 0, "the window is hidden");
            let ex_style = GetWindowLongPtrW(hwnd, GWL_EXSTYLE) as u32;
            assert!(ex_style & WS_EX_LAYERED != 0);
            let mut rect = RECT {
                left: 0,
                top: 0,
                right: 0,
                bottom: 0,
            };
            GetWindowRect(hwnd, &mut rect);
            assert_eq!(
                (rect.right - rect.left, rect.bottom - rect.top),
                (first.width, first.height)
            );
            assert!(
                !MonitorFromRect(&rect, MONITOR_DEFAULTTONULL).is_null(),
                "the window at {},{} is on no monitor",
                rect.left,
                rect.top
            );
            // With a shell, the desktop is below the window, so the desktop is not drawn over it.
            if let Some(desktop) = win::desktop_window() {
                let mut next = GetWindow(hwnd, GW_HWNDNEXT);
                let mut steps = 0;
                while !next.is_null() && next != desktop && steps < 4096 {
                    next = GetWindow(next, GW_HWNDNEXT);
                    steps += 1;
                }
                assert_eq!(next, desktop, "the desktop is drawn over the window");
            }
        }

        // A widget's own image: its left half drawn, its right half see-through, as a clock's
        // text over the wallpaper. The system is handed exactly that, alpha and all.
        let frame = |png: Vec<u8>| {
            WindowCommand::Frame(WidgetFrame {
                widget_id: 4242,
                width: 8,
                height: 2,
                png,
            })
        };
        let drawn = [[255u8, 255, 255, 255]; 4]
            .into_iter()
            .chain([[0u8, 0, 0, 0]; 4])
            .collect::<Vec<_>>();
        windows.command(frame(rgba_png(8, 2, &[drawn.clone(), drawn].concat())));
        let image = next_presented(1, deadline);
        assert!(
            image.committed,
            "the system did not take the widget's image"
        );
        assert!(
            image.opaque * 10 >= pixels * 4 && image.opaque * 10 <= pixels * 6,
            "{} of {pixels} pixels are opaque; half the widget's image is see-through",
            image.opaque
        );
        assert!(
            image.clear * 10 >= pixels * 4,
            "only {} of {pixels} pixels are see-through; the widget's transparent half is drawn opaque",
            image.clear
        );

        // Its app stops drawing and the image comes back empty: the last one stays, not the
        // placeholder.
        windows.command(frame(rgba_png(8, 2, &[[0u8; 4]; 16])));
        let kept = next_presented(2, deadline);
        assert!(
            kept.opaque == image.opaque && kept.clear == image.clear,
            "after an empty image the window shows {} opaque and {} clear pixels, not its last image ({} and {})",
            kept.opaque,
            kept.clear,
            image.opaque,
            image.clear
        );
        windows.command(WindowCommand::Remove(4242));
    }

    /// The window's next drawing after the first `seen` ones.
    #[cfg(windows)]
    fn next_presented(seen: usize, deadline: Instant) -> win::Presented {
        let deadline = deadline.max(Instant::now() + Duration::from_secs(10));
        loop {
            if let Some(found) = win::PRESENTED.lock().unwrap().get(seen).copied() {
                return found;
            }
            assert!(
                Instant::now() < deadline,
                "the widget's window was not drawn again"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// An RGBA PNG of `width` by `height` straight-alpha `pixels`, stored without compression.
    #[cfg(windows)]
    fn rgba_png(width: u32, height: u32, pixels: &[[u8; 4]]) -> Vec<u8> {
        fn crc(bytes: &[u8]) -> u32 {
            let mut crc = 0xffff_ffffu32;
            for &b in bytes {
                crc ^= b as u32;
                for _ in 0..8 {
                    crc = if crc & 1 != 0 {
                        (crc >> 1) ^ 0xedb8_8320
                    } else {
                        crc >> 1
                    };
                }
            }
            !crc
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8], body: &[u8]) {
            out.extend_from_slice(&(body.len() as u32).to_be_bytes());
            let start = out.len();
            out.extend_from_slice(kind);
            out.extend_from_slice(body);
            let sum = crc(&out[start..]);
            out.extend_from_slice(&sum.to_be_bytes());
        }
        let mut raw = Vec::new();
        for row in pixels.chunks(width as usize) {
            raw.push(0);
            for p in row {
                raw.extend_from_slice(p);
            }
        }
        let (mut a, mut b) = (1u32, 0u32);
        for &byte in &raw {
            a = (a + byte as u32) % 65521;
            b = (b + a) % 65521;
        }
        let len = raw.len() as u16;
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&len.to_le_bytes());
        z.extend_from_slice(&(!len).to_le_bytes());
        z.extend_from_slice(&raw);
        z.extend_from_slice(&((b << 16) | a).to_be_bytes());
        let mut header = Vec::new();
        header.extend_from_slice(&width.to_be_bytes());
        header.extend_from_slice(&height.to_be_bytes());
        header.extend_from_slice(&[8, 6, 0, 0, 0]);
        let mut out = vec![0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a];
        chunk(&mut out, b"IHDR", &header);
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }
}
