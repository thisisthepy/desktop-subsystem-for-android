//! Task windows on macOS: one `NSWindow` per Android task.
//!
//! Each app's windows belong to its window host ([`window_host`]), a copy of
//! `dsa-app-window` inside the app's own bundle, so the Dock shows each app as itself. The
//! window manager (`dsa-wm`, [`crate::wm`]) runs gfxstream's host renderer ([`renderer`]),
//! which copies each task's frames on the GPU into shared `IOSurface`s ([`surface`]), or
//! reads them back where it cannot; the surfaces reach the window host as Mach ports
//! ([`surface_port`]) and the window host shows them.
//!
//! Each window's content view is backed by a `CAMetalLayer`. A frame is copied on the GPU
//! from its surface into the layer's next drawable and presented; the drawable is the size
//! of the frame's content and the layer never scales it, so while the guest catches up with
//! a resize the window shows the last frame one to one rather than stretched ([`sizing`]).

pub mod frame_gate;
pub mod frames;
pub mod screenshot;
pub mod sizing;
pub mod title_bar;
pub mod window_menu;

#[cfg(target_os = "macos")]
pub mod dock_menu;
#[cfg(target_os = "macos")]
pub mod menu_bar;
#[cfg(target_os = "macos")]
pub mod metal;
#[cfg(target_os = "macos")]
pub mod renderer;
#[cfg(target_os = "macos")]
pub mod surface;
#[cfg(target_os = "macos")]
pub mod surface_port;
#[cfg(target_os = "macos")]
pub mod window;
#[cfg(target_os = "macos")]
pub mod window_host;
