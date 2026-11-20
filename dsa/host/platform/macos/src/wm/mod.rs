//! The window manager on macOS, `dsa-wm`: the user's compositor.
//!
//! It runs as the user, started by its launchd agent (or by `dsa-hostd`), and owns no app's
//! window: only the panels of the widgets placed on the desktop, which its main thread runs as
//! an accessory app (no Dock icon). It starts the graphics renderer (`dsa-gfx`, gfxstream on MoltenVK, in a process of its own:
//! see `crate::gfx`), which serves the guest's graphics streams, and starts it again whenever
//! it ends. It follows the guest's tasks through the service's local API, starts one window host
//! (`dsa-app-window`, in the app's own bundle) per app, and hands each task's frames to its
//! window host in shared `IOSurface`s, paced so that the guest never waits for a window.
//!
//! - [`manager`]: which task has a window and where each display's frames go (platform
//!   neutral, tested on every build host).
//! - [`frame_slots`]: the pacing of one task's frames between renderer and window host.
//! - [`launch`]: starting an app's window host from its bundle.
//! - `hosts`, `present`, `graphics`, `run` (macOS only): the window hosts' connections, the
//!   frames' way to them, the graphics streams, and the loop that runs it all.
//! - `notifications` (macOS only): the user's notifications, and DSA's own news about updates,
//!   in the Notification Center.
//! - [`widget_panels`]: what a widget's panel is on the desktop, its units, input and menu
//!   (platform neutral); `widgets` (macOS only): the panels themselves.

pub mod frame_slots;
pub mod launch;
pub mod manager;
pub mod widget_panels;

#[cfg(target_os = "macos")]
pub mod graphics;
#[cfg(target_os = "macos")]
pub mod hosts;
#[cfg(target_os = "macos")]
pub mod notifications;
#[cfg(target_os = "macos")]
pub mod present;
#[cfg(target_os = "macos")]
pub mod run;
#[cfg(target_os = "macos")]
pub mod widgets;

/// Writes a line to the window manager's log (standard error, which its launchd agent keeps
/// in `logs/dsa-wm.log` in the user's DSA folder).
pub fn log(line: &str) {
    eprintln!("dsa-wm: {line}");
}
