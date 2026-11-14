//! The Linux compositor: one Wayland toplevel per Android task.
//!
//! - [`toplevel`]: a task window's sizes, scale, mode and title bar, with no Wayland in it.
//! - [`frames`]: the task frames the renderer reads back, on their way to the windows.
//! - [`dmabuf_read`]: GPU frames read back to memory in any layout, for the title bar's band
//!   and a screenshot (Linux only).
//! - [`wayland`]: the toplevels, their buffers and the seat (Linux only).
//! - [`gfxstream`]: the host renderer and the guest's graphics streams (Linux only).
//! - [`title_bar`]: what a task window's title bar shows and what its points do.
//! - [`window_menu`]: a task window's menu, its items and how it is drawn.
//! - [`widget_menu`]: a widget window's menu, laid out and drawn as the window menu is.
//! - [`widget_windows`]: what a widget window is on this desktop, and its units and input.
//! - [`text`]: text for the title bar and the menu, drawn with Pango (Linux only).
//! - [`session`]: what ties them to the DSA service (Linux only).

#[cfg(target_os = "linux")]
pub mod dmabuf_read;
pub mod frames;
pub mod title_bar;
pub mod toplevel;
pub mod widget_menu;
pub mod widget_windows;
pub mod window_menu;

#[cfg(target_os = "linux")]
pub mod gfxstream;
#[cfg(target_os = "linux")]
pub mod session;
#[cfg(target_os = "linux")]
pub mod text;
#[cfg(target_os = "linux")]
pub mod wayland;
#[cfg(target_os = "linux")]
pub mod x11_pip;
