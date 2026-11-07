//! DSA Settings for Linux: the settings app of the Linux host, in the style of GNOME
//! Settings.
//!
//! The app is a thin client of the local API. It asks the DSA service through the same
//! client the `dsa` command is ([`api`]), reads the JSON that client prints ([`model`]),
//! and turns it into what the pages show ([`view`]). It checks, stores and works out
//! nothing of its own, so the Linux, Windows and macOS settings apps behave the same.
//!
//! Only the window ([`ui`]) needs GTK 4 and libadwaita, and only Linux builds it. Everything
//! else builds and tests on any host, without a display.

pub mod api;
pub mod i18n;
pub mod model;
pub mod pages;
pub mod view;

#[cfg(target_os = "linux")]
pub mod ui;

/// The app's ID: its D-Bus name, its `.desktop` file and its AppStream component.
pub const APP_ID: &str = "io.github.thisisthepy.DSA.Settings";

/// The gettext domain of the app's translations.
pub const TEXT_DOMAIN: &str = "dsa-settings";
