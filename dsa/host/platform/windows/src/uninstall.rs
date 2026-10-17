//! What uninstalling needs from dsa-hostd: removing the Android images and the setup
//! files the service downloaded, which the installer did not put there and so does not
//! know about.

use std::path::{Path, PathBuf};

use dsa_host_core::update::program::Version;

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

/// The folder an installer passed on the command line. Windows Installer folder paths end
/// in a backslash, and `"C:\...\images\"` on a command line turns the final `\"` into an
/// escaped quote, so the argument arrives as `C:\...\images"`. The quote and any trailing
/// separators are not part of the folder.
pub fn installer_folder(argument: &str) -> PathBuf {
    PathBuf::from(argument.trim_end_matches('"').trim_end_matches(['\\', '/']))
}
