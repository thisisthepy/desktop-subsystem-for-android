//! What uninstalling needs from dsa-hostd: removing the Android images the service
//! downloaded, which the installer did not put there and so does not know about.

use std::path::Path;

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
