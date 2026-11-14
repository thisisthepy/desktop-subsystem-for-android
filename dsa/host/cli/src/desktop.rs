//! `dsa apps desktop-shortcut <package>`: a shortcut to an app on the user's desktop, as the
//! desktop operating system makes them, which the settings apps' "Create desktop shortcut"
//! runs. It starts the app as its Start menu entry (launcher entry, Launchpad bundle) does,
//! and it goes when the app is uninstalled.

use std::path::PathBuf;

use dsa_host_core::tasks::AppInfo;

/// Puts a shortcut to `app` on the desktop; returns where it is. On Windows it is a `.lnk`
/// that runs the app's window program, as its Start menu shortcut does.
#[cfg(windows)]
pub fn place_on_desktop(app: &AppInfo) -> Result<PathBuf, String> {
    dsa_host_windows::desktop_shortcuts::place_app_on_desktop(app)
}

/// Puts a shortcut to `app` on the desktop; returns where it is. On Linux it is a copy of the
/// app's launcher entry in the desktop folder `xdg-user-dirs` names.
#[cfg(target_os = "linux")]
pub fn place_on_desktop(app: &AppInfo) -> Result<PathBuf, String> {
    dsa_host_linux::shell::desktop_entry::place_on_desktop(app)
}

/// Puts a shortcut to `app` on the desktop; returns where it is. On macOS it is a link on the
/// Desktop to the app's bundle in `~/Applications/DSA Apps`.
#[cfg(target_os = "macos")]
pub fn place_on_desktop(app: &AppInfo) -> Result<PathBuf, String> {
    dsa_host_macos::shell::app_stubs::place_on_desktop(&app.package_name)
}

/// Puts a shortcut to `app` on the desktop; returns where it is.
#[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
pub fn place_on_desktop(app: &AppInfo) -> Result<PathBuf, String> {
    let _ = app;
    Err("desktop shortcuts are not made on this computer's system".into())
}
