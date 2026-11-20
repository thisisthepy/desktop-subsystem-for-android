//! Which image the next boot starts from: the update store's current image when there is a
//! store, else the image folder the configuration names.

use std::fs;
use std::path::{Path, PathBuf};

use super::store::ImageStore;

/// The image the next boot starts from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CurrentImage {
    /// The image's folder, which holds its `image.json`.
    pub dir: PathBuf,
    /// The image's version: the store's current one, or the one `image.json` names. `None`
    /// for a folder whose `image.json` is missing or names no version.
    pub version: Option<String>,
    /// The Android major version of the store's current image. `None` without a store, which
    /// keeps no record of it.
    pub android_major: Option<u32>,
}

impl CurrentImage {
    /// The name a cache folder or a recorded disk version takes: the version, or `local`
    /// for an image that names none.
    pub fn cache_key(&self) -> &str {
        self.version.as_deref().unwrap_or("local")
    }
}

/// The image the next boot uses. With a `store` it is the store's current image, which an
/// update changes while the service runs; without one it is the folder `configured_dir`.
pub fn current_image(
    store: Option<&ImageStore>,
    configured_dir: Option<&Path>,
) -> Result<CurrentImage, String> {
    if let Some(store) = store {
        let current = store.state().map_err(|e| e.to_string())?.current;
        return Ok(CurrentImage {
            dir: store.image_dir(&current.version),
            version: Some(current.version),
            android_major: Some(current.android_major),
        });
    }
    let dir = configured_dir.ok_or("the configuration names no image to boot")?;
    let version = fs::read_to_string(dir.join("image.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<serde_json::Value>(&text).ok())
        .and_then(|manifest| manifest["version"].as_str().map(str::to_string));
    Ok(CurrentImage {
        dir: dir.to_path_buf(),
        version,
        android_major: None,
    })
}

/// Removes the copies of images other than `keep` from `cache_root`, the folder that holds
/// one folder per image version.
pub fn prune_image_cache(cache_root: &Path, keep: &str) {
    if let Ok(entries) = fs::read_dir(cache_root) {
        for entry in entries.flatten() {
            if entry.file_name().to_string_lossy() != keep {
                let _ = fs::remove_dir_all(entry.path());
            }
        }
    }
}
