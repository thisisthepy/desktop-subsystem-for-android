//! Which monitor an app's window opens on, and where on it.
//!
//! The window manager and the app's window host both read the answer before the window opens:
//! the window manager to have the task draw at that monitor's density and size first, the
//! window host to put the window there. A window placed only after it opened showed the app at
//! the main monitor's density and size, then shrank on a monitor of a lower scale while the app
//! redrew. Each side asks [`opening_monitor`] with the monitors the desktop has now, so both
//! come to the same one.
//!
//! The monitor an app's last window closed on is remembered for it, in a small file of its own
//! beside the places of its overlay windows ([`remember_monitor`], [`remembered_monitor`]), so
//! the app opens there again the next time, also after DSA or Windows started again. A monitor
//! that is not connected any more is passed over for the main one.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::window_hosts::is_valid_package;

/// A monitor of the desktop, as the platform describes it: an ID that names the same monitor
/// whatever order the desktop lists the monitors in, its DPI (96 is 100%), whether it is the
/// main one, and its work area (left, top, right, bottom), the part a window is placed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopMonitor {
    pub id: String,
    pub dpi: u32,
    pub main: bool,
    pub work: Area,
}

/// A rectangle on the desktop: left, top, right, bottom.
pub type Area = (i32, i32, i32, i32);

/// The monitor an app's window opens on: the one its last window closed on (`last`, by ID)
/// while that one is connected, else the main monitor. `None` only when the desktop has no
/// monitor.
pub fn opening_monitor<'a>(
    last: Option<&str>,
    monitors: &'a [DesktopMonitor],
) -> Option<&'a DesktopMonitor> {
    last.filter(|id| !id.is_empty())
        .and_then(|id| monitors.iter().find(|monitor| monitor.id == id))
        .or_else(|| monitors.iter().find(|monitor| monitor.main))
        .or_else(|| monitors.first())
}

/// The longest monitor ID kept: a Windows device interface path is under 200 characters.
pub const MAX_MONITOR_ID: usize = 512;

/// What is remembered of an app's last window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct LastWindow {
    /// The ID of the monitor it closed on.
    monitor: String,
}

/// Whether `id` can be a monitor's ID: not empty, not too long, one line of text.
fn usable_id(id: &str) -> bool {
    !id.is_empty() && id.len() <= MAX_MONITOR_ID && !id.chars().any(char::is_control)
}

/// The file the monitor of `package`'s last window is remembered in, in `dir`; `None` for a
/// name that is no package.
fn record_file(dir: &Path, package: &str) -> Option<PathBuf> {
    is_valid_package(package).then(|| dir.join(format!("{package}.monitor.json")))
}

/// The ID of the monitor `package`'s last window closed on, as remembered in `dir`, if one was
/// and it can be read.
pub fn remembered_monitor(dir: &Path, package: &str) -> Option<String> {
    let path = record_file(dir, package)?;
    let last = crate::settings::read_json::<LastWindow>(&path)
        .ok()
        .flatten()?;
    usable_id(&last.monitor).then_some(last.monitor)
}

/// Remembers, in `dir`, that `package`'s last window closed on the monitor `id`.
pub fn remember_monitor(dir: &Path, package: &str, id: &str) -> Result<(), String> {
    if !usable_id(id) {
        return Err(format!(
            "{id:?} is not a monitor's ID; it is not remembered"
        ));
    }
    let path = record_file(dir, package).ok_or_else(|| {
        format!("{package} is not a package name; its window's monitor is not remembered")
    })?;
    if remembered_monitor(dir, package).as_deref() == Some(id) {
        return Ok(());
    }
    crate::settings::write_json(
        &path,
        &LastWindow {
            monitor: id.to_string(),
        },
    )
}

/// Where a window goes on the monitor whose work area is `to`, when the desktop put it at the
/// top left corner `at` on the monitor whose work area is `from`, with an outer size of `size`:
/// as far in from `to`'s top left as it was from `from`'s, moved back in where it would reach
/// past `to`'s right or bottom edge, and at `to`'s top left when it is larger than `to`.
pub fn placed_on(at: (i32, i32), size: (i32, i32), from: Area, to: Area) -> (i32, i32) {
    let along = |at: i32, from_start: i32, to_start: i32, to_end: i32, length: i32| {
        let wanted = to_start + (at - from_start).max(0);
        wanted.min(to_end - length).max(to_start)
    };
    (
        along(at.0, from.0, to.0, to.2, size.0),
        along(at.1, from.1, to.1, to.3, size.1),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn monitor(id: &str, dpi: u32, main: bool, work: Area) -> DesktopMonitor {
        DesktopMonitor {
            id: id.into(),
            dpi,
            main,
            work,
        }
    }

    /// The owner's monitors: a 200% main one and two 125% ones, which have the same size and
    /// scale and differ only by their IDs.
    fn owners() -> Vec<DesktopMonitor> {
        vec![
            monitor("main", 192, true, (0, 0, 2880, 1752)),
            monitor("left", 120, false, (-2560, 0, 0, 1392)),
            monitor("right", 120, false, (2880, 0, 5440, 1392)),
        ]
    }

    #[test]
    fn fr4_a_window_opens_on_the_main_monitor_when_nothing_else_is_known() {
        let monitors = owners();
        assert_eq!(opening_monitor(None, &monitors).unwrap().id, "main");
        assert_eq!(opening_monitor(Some(""), &monitors).unwrap().id, "main");
        // A desktop that names no main monitor opens on its first.
        let mut unnamed = owners();
        unnamed[0].main = false;
        assert_eq!(opening_monitor(None, &unnamed).unwrap().id, "main");
        assert_eq!(opening_monitor(None, &[]), None);
    }

    #[test]
    fn fr4_an_app_opens_again_on_the_monitor_its_last_window_closed_on() {
        let monitors = owners();
        // The two 125% monitors look alike; only the ID tells them apart.
        assert_eq!(
            opening_monitor(Some("right"), &monitors).unwrap().id,
            "right"
        );
        assert_eq!(opening_monitor(Some("left"), &monitors).unwrap().id, "left");
        // That monitor is not connected any more: the main one.
        let unplugged: Vec<DesktopMonitor> = monitors
            .iter()
            .filter(|monitor| monitor.id != "right")
            .cloned()
            .collect();
        assert_eq!(
            opening_monitor(Some("right"), &unplugged).unwrap().id,
            "main"
        );
        // Whatever order the desktop lists them in.
        let mut reordered = owners();
        reordered.reverse();
        assert_eq!(
            opening_monitor(Some("left"), &reordered).unwrap().id,
            "left"
        );
    }

    #[test]
    fn fr4_the_monitor_an_apps_last_window_closed_on_is_remembered_for_it() {
        let dir = std::env::temp_dir().join(format!(
            "dsa-window-monitor-{}-{:?}",
            std::process::id(),
            std::thread::current().id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let flo = "skplanet.musicmate";
        assert_eq!(remembered_monitor(&dir, flo), None);
        let id = r"\\?\DISPLAY#GSM5B09#5&2bd7ba7f&0&UID4352#{e6f07b5f-ee97-4a90-b076-33f57bf4eaa7}";
        remember_monitor(&dir, flo, id).unwrap();
        assert_eq!(remembered_monitor(&dir, flo).as_deref(), Some(id));
        // Each app has its own.
        assert_eq!(remembered_monitor(&dir, "com.example.notes"), None);
        remember_monitor(&dir, flo, r"\\.\DISPLAY2").unwrap();
        assert_eq!(
            remembered_monitor(&dir, flo).as_deref(),
            Some(r"\\.\DISPLAY2")
        );
        // Nothing is kept for a name that is no package, or an ID that is no ID.
        assert!(remember_monitor(&dir, "../evil", id).is_err());
        assert!(remember_monitor(&dir, flo, "").is_err());
        assert!(remember_monitor(&dir, flo, "two\nlines").is_err());
        assert!(remember_monitor(&dir, flo, &"x".repeat(MAX_MONITOR_ID + 1)).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn fr4_a_window_moved_onto_another_monitor_keeps_its_place_in_it() {
        // Where the desktop put it on the main monitor, at the same offset on the right one.
        assert_eq!(
            placed_on(
                (100, 80),
                (800, 600),
                (0, 0, 2880, 1752),
                (2880, 0, 5440, 1392)
            ),
            (2980, 80)
        );
        // Kept inside, and at the top left when it is larger than the monitor.
        assert_eq!(
            placed_on(
                (2400, 1200),
                (800, 600),
                (0, 0, 2880, 1752),
                (2880, 0, 5440, 1392)
            ),
            (5440 - 800, 1392 - 600)
        );
        assert_eq!(
            placed_on(
                (10, 10),
                (3000, 2000),
                (0, 0, 2880, 1752),
                (-2560, 0, 0, 1392)
            ),
            (-2560, 0)
        );
    }
}
