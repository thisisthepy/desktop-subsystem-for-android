//! What the desktop shows of the Android apps, and what it hands them.
//!
//! - [`desktop_entry`]: a launcher entry and an icon for each installed app, so the apps sit
//!   in the desktop's app menu and its dock as themselves.
//! - [`notifications`]: the guest's notifications as the desktop's
//!   (`org.freedesktop.Notifications`), and the user's clicks back.
//! - [`file_drop`]: files dropped on a task window (`text/uri-list`).
//! - [`file_drag`]: files an app drags out of its window, as the desktop takes them.
//! - [`clipboard`]: which clipboard formats carry text.
//! - [`png`]: PNG images in and out, for the title bar icons and screenshots.
//! - [`screenshot`]: where a window's screenshot is saved, and what of the frame it keeps.
//! - [`pin_shortcuts`]: an app's request to pin a shortcut, asked as a notification.
//! - [`dbus`]: the session bus side of the notifications (Linux only).

pub mod clipboard;
pub mod desktop_entry;
pub mod file_drag;
pub mod file_drop;
pub mod notifications;
pub mod pin_shortcuts;
pub mod png;
pub mod screenshot;

#[cfg(target_os = "linux")]
pub mod dbus;
