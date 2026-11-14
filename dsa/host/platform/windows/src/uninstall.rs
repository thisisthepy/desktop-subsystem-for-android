//! What uninstalling needs from dsa-hostd: removing the Android images and the setup
//! files the service downloaded, the settings, logs and other files the service made in the
//! machine's DSA folder, and in each user's profile the Start menu shortcuts and the files the
//! window manager made, which the installer did not put there and so does not know about.
//! Each user's Android disk stays.

use std::path::{Path, PathBuf};

use dsa_host_core::update::program::Version;

use crate::paths::{MachineRoot, UserRoot};

/// Removes the update store at `dir` (the `images` folder of the service's data folder).
///
/// It refuses any folder that does not look like that store, so a wrong path can never
/// remove something else: the folder must be named `images` and hold nothing but the
/// store's `state.json` (and its temporary twin) and its `images` folder.
pub fn remove_downloaded_images(dir: &Path) -> Result<(), String> {
    if dir.file_name().map(|n| n != "images").unwrap_or(true) {
        return Err(format!("{} is not an images folder", dir.display()));
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("reading {}: {error}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !matches!(name.as_str(), "state.json" | "state.json.new" | "images") {
            return Err(format!(
                "{} holds {name:?}, which the image store never makes, so it is left alone",
                dir.display()
            ));
        }
    }
    std::fs::remove_dir_all(dir).map_err(|e| format!("removing {}: {e}", dir.display()))
}

/// Removes the folder of downloaded setup files at `dir` (the `updates` folder of the
/// service's data folder).
///
/// Like [`remove_downloaded_images`] it refuses any folder that does not look like that
/// one: it must be named `updates` and hold nothing but the update state (and its
/// temporary twin), downloads in progress (`.download-<version>`) and folders named after
/// versions.
pub fn remove_downloaded_updates(dir: &Path) -> Result<(), String> {
    if dir.file_name().map(|n| n != "updates").unwrap_or(true) {
        return Err(format!("{} is not an updates folder", dir.display()));
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("reading {}: {error}", dir.display())),
    };
    for entry in entries {
        let entry = entry.map_err(|e| format!("reading {}: {e}", dir.display()))?;
        let name = entry.file_name().to_string_lossy().into_owned();
        let ours = matches!(name.as_str(), "state.json" | "state.json.new")
            || name.starts_with(".download-")
            || Version::parse(&name).is_ok();
        if !ours {
            return Err(format!(
                "{} holds {name:?}, which program updates never make, so it is left alone",
                dir.display()
            ));
        }
    }
    std::fs::remove_dir_all(dir).map_err(|e| format!("removing {}: {e}", dir.display()))
}

/// Removes the Start menu shortcuts the window manager made for one user: the ones its note
/// (`start-menu.json` in the user's `%LOCALAPPDATA%\DSA`) names, in the user's Start menu
/// `programs` folder, and then the note. A shortcut the note does not name is left alone, and
/// so is a name that is not a plain `.lnk` file name, so a damaged note cannot remove anything
/// else. Returns how many shortcuts were removed; a user with no note has none.
pub fn remove_start_menu_shortcuts(note: &Path, programs: &Path) -> Result<usize, String> {
    let text = match std::fs::read_to_string(note) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("reading {}: {error}", note.display())),
    };
    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("reading {}: {e}", note.display()))?;
    let mut removed = 0;
    if let Some(shortcuts) = parsed.get("shortcuts").and_then(|s| s.as_object()) {
        for file in shortcuts.values().filter_map(|file| file.as_str()) {
            let plain = !file.contains(['\\', '/', ':'])
                && file.to_ascii_lowercase().ends_with(".lnk")
                && file != ".lnk";
            if !plain {
                continue;
            }
            match std::fs::remove_file(programs.join(file)) {
                Ok(()) => removed += 1,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
                Err(error) => return Err(format!("removing {file}: {error}")),
            }
        }
    }
    std::fs::remove_file(note).map_err(|e| format!("removing {}: {e}", note.display()))?;
    Ok(removed)
}

/// Removes the desktop shortcuts to apps that the window manager and `dsa` made for one user:
/// the ones its note (`desktop-shortcuts.json` in the user's `%LOCALAPPDATA%\DSA`) names, and
/// then the note. Only absolute paths to `.lnk` files are removed, so a damaged note cannot
/// remove anything else. Returns how many shortcuts were removed; a user with no note has none.
pub fn remove_desktop_shortcuts(note: &Path) -> Result<usize, String> {
    let text = match std::fs::read_to_string(note) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(0),
        Err(error) => return Err(format!("reading {}: {error}", note.display())),
    };
    let parsed: serde_json::Value =
        serde_json::from_str(&text).map_err(|e| format!("reading {}: {e}", note.display()))?;
    let mut paths = Vec::new();
    for made in parsed
        .get("apps")
        .and_then(|apps| apps.as_object())
        .into_iter()
        .flat_map(|apps| apps.values())
    {
        paths.extend(made.get("app").and_then(|path| path.as_str()));
        if let Some(shortcuts) = made.get("shortcuts").and_then(|s| s.as_object()) {
            paths.extend(shortcuts.values().filter_map(|path| path.as_str()));
        }
    }
    let mut removed = 0;
    for path in paths {
        let plain = is_absolute_windows_path(path)
            && path.to_ascii_lowercase().ends_with(".lnk")
            && !path.ends_with("\\.lnk");
        if !plain {
            continue;
        }
        match std::fs::remove_file(path) {
            Ok(()) => removed += 1,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => return Err(format!("removing {path}: {error}")),
        }
    }
    std::fs::remove_file(note).map_err(|e| format!("removing {}: {e}", note.display()))?;
    Ok(removed)
}

/// Whether `path` is an absolute Windows path (`C:\...` or `\\server\...`), told by its text so
/// the check is the same on every system the tests run on.
fn is_absolute_windows_path(path: &str) -> bool {
    let bytes = path.as_bytes();
    let drive =
        bytes.len() > 3 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':' && bytes[2] == b'\\';
    drive || path.starts_with("\\\\")
}

fn remove_entry(path: &Path) -> Result<(), String> {
    let result = if path.is_dir() {
        std::fs::remove_dir_all(path)
    } else {
        std::fs::remove_file(path)
    };
    match result {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("removing {}: {error}", path.display())),
    }
}

/// Removes one user's DSA files: the Start menu shortcuts the window manager made (in
/// `programs`) and the desktop shortcuts to apps, then everything in their DSA folder but the
/// Android disk and the image version recorded with it. The folder itself goes when nothing is
/// left in it. Returns what could not be removed, to report; the rest is removed either way.
pub fn remove_user_files(root: &UserRoot, programs: &Path) -> Vec<String> {
    let mut problems = Vec::new();
    if let Err(why) = remove_start_menu_shortcuts(&root.start_menu_note(), programs) {
        problems.push(why);
    }
    if let Err(why) = remove_desktop_shortcuts(&root.desktop_shortcuts_note()) {
        problems.push(why);
    }
    if let Ok(entries) = std::fs::read_dir(root.dir()) {
        for entry in entries.filter_map(Result::ok) {
            let name = entry.file_name().to_string_lossy().into_owned();
            if UserRoot::kept_on_uninstall(&name) {
                continue;
            }
            if let Err(why) = remove_entry(&entry.path()) {
                problems.push(why);
            }
        }
    }
    let _ = std::fs::remove_dir(root.dir());
    problems
}

/// Removes what the service made in the machine's DSA folder besides the images and the
/// downloaded setup files (which [`remove_downloaded_images`] and [`remove_downloaded_updates`]
/// remove): the settings, the logs, the `tmp` folder, the direct-mapped mode's system disks and
/// the trace switch files of earlier
/// releases. A `users` folder, where the first releases kept each user's Android disk, stays,
/// since a disk that could not be moved into its user's profile is still there. Returns what
/// could not be removed.
pub fn remove_machine_files(root: &MachineRoot) -> Vec<String> {
    let mut paths = vec![
        root.settings(),
        root.logs(),
        root.tmp(),
        root.direct_mapped(),
    ];
    paths.extend(root.old_trace_markers());
    paths
        .iter()
        .filter_map(|path| remove_entry(path).err())
        .collect()
}

/// The folder an installer passed on the command line. Windows Installer folder paths end
/// in a backslash, and `"C:\...\images\"` on a command line turns the final `\"` into an
/// escaped quote, so the argument arrives as `C:\...\images"`. The quote and any trailing
/// separators are not part of the folder.
pub fn installer_folder(argument: &str) -> PathBuf {
    PathBuf::from(argument.trim_end_matches('"').trim_end_matches(['\\', '/']))
}
