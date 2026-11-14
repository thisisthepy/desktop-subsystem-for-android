//! The widgets of the user's Android as borderless windows on the user's desktop, as every
//! host shows them.
//!
//! A host's window manager watches the widgets placed on the user's desktop over the local API
//! and shows each one in a window of its own that belongs to the desktop: below every other
//! window, and in neither the task bar nor the window switcher ([`service::sync`]). The window
//! shows the widget's latest frame, a PNG image with an alpha channel, so a widget with rounded
//! corners or a see-through background looks as it does on a phone. The image is Android's at
//! its own size and is never scaled ([`geometry::unscaled`]). Nothing is drawn over the
//! widget, not even while the pointer is over it, so the widget's own buttons stay free to
//! click in every corner. A right click opens the widget's menu at the pointer, and the menu
//! key opens it under the widget ([`menu`]).
//!
//! A press, a move and a release inside the widget go to the widget's view in Android, so its
//! buttons and lists work and an app it opens shows as a window. A press that moves before
//! [`geometry::HOLD_TO_MOVE`] is the widget's own (a list scrolls), and a tap stays a tap
//! ([`gesture::Gesture`]).
//!
//! A long press anywhere on the widget edits it as Android's home screen does: a finger or a
//! pen held on it, or the mouse's left button held, for [`geometry::HOLD_TO_MOVE`] without
//! moving past the drag threshold. Android's press is cancelled then, so the long press never
//! also clicks. The widget lifts: its image, at its own size, rises a little over a soft
//! shadow, with a border in the accent color around it and a handle on each edge it may be
//! resized along ([`geometry::edit_layout`]). Dragging on moves it; letting go without moving
//! opens its menu under it (Widget settings, Resize, Remove). While it is edited, dragging it
//! moves it, a click on it opens the menu again, its handles and edges resize it within what
//! the widget allows ([`geometry::edit_hit_test`]), and Esc, a click anywhere else, or the end
//! of a move or a resize ends the edit. The window is also resized by its edges when it is
//! not edited ([`geometry::hit_test`]). While it is resized, its size goes to Android when the
//! drag pauses and when it ends ([`gesture::ResizeReports`]), and its last image shows
//! unscaled meanwhile. The service is told where the window is and how large, in its
//! monitor's pixels and Android's density there; the service remembers it and tells Android,
//! which draws the widget at that size and tells the widget's app its size in dp, so the app
//! can pick the layout that fits.
//!
//! - [`geometry`]: where the parts of a widget window are, at a monitor's scale.
//! - [`gesture`]: what a press is doing, and when a resize passes its size on.
//! - [`menu`]: what the widget's menu offers.
//! - [`service`]: the commands and reports between the windows and the service, and the watch.
//! - [`desk`]: the widget windows' behaviour for a host that draws them itself (Linux, macOS):
//!   everything but the windows themselves, which the host makes and moves as it is told.
//! - [`paint`]: their pictures, drawn on the CPU.
//!
//! The Windows host keeps its own window procedure and its Fluent drawing, on top of the same
//! geometry, gestures, menu and watch.

pub mod desk;
pub mod geometry;
pub mod gesture;
pub mod menu;
pub mod paint;
pub mod service;

pub use geometry::*;
pub use gesture::*;
pub use service::*;
