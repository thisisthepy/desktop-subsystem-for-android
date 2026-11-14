//! The host is the shell: what Android apps show of themselves on a Mac outside their
//! windows.
//!
//! - [`app_stubs`]: an app bundle per Android app in `~/Applications/DSA Apps`, so each app
//!   is in Launchpad and Spotlight and can be kept in the Dock.
//! - [`icns`]: the app's icon as an `.icns` file for its bundle.
//! - [`notifications`]: how a guest notification becomes a `UNNotificationRequest`, and
//!   (macOS only) the notification center itself.
//! - [`pin_shortcuts`]: an app's request to pin a shortcut, asked as a banner with buttons.
//! - `clipboard`, `workspace` (macOS only): the pasteboard, and opening links and files.
//!
//! Drag and drop is the task view's ([`crate::input`]); full screen and picture in picture
//! are the task window's ([`crate::compositor`]).

pub mod app_stubs;
pub mod icns;
pub mod notifications;
pub mod pin_shortcuts;

#[cfg(target_os = "macos")]
pub mod clipboard;
#[cfg(target_os = "macos")]
pub mod workspace;
