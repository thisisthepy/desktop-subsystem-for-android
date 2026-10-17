//! Which image to move to.

use serde::{Deserialize, Serialize};

use super::manifest::{ImageEntry, Manifest};

/// The image a user runs now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstalledImage {
    pub version: String,
    pub android_major: u32,
    pub build: u64,
}

impl InstalledImage {
    pub fn of(entry: &ImageEntry) -> InstalledImage {
        InstalledImage {
            version: entry.version.clone(),
            android_major: entry.android_major,
            build: entry.build,
        }
    }
}

/// What to do after reading the release list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// Nothing newer to move to.
    UpToDate,
    /// Move to this image.
    Apply(ImageEntry),
    /// Nothing to move to, and the installed image is no longer supported. The text says
    /// so in words a person can act on.
    Unsupported(String),
}

/// Checks that moving from `installed` to `image` is allowed. The reason names the rule.
pub fn check(
    installed: &InstalledImage,
    image: &ImageEntry,
    manifest: &Manifest,
    failed: &[String],
) -> Result<(), String> {
    if !manifest.supported_majors.contains(&image.android_major) {
        return Err(format!(
            "Android {} ({}) is not a supported version (supported: {})",
            image.android_major,
            image.version,
            majors(manifest)
        ));
    }
    if image.android_major < installed.android_major {
        return Err(format!(
            "{} is Android {}, older than the installed Android {}; Android cannot move user \
             data to an older major version",
            image.version, image.android_major, installed.android_major
        ));
    }
    if (image.android_major, image.build) <= (installed.android_major, installed.build) {
        return Err(format!(
            "{} is not newer than the installed {}",
            image.version, installed.version
        ));
    }
    if failed.contains(&image.version) {
        return Err(format!(
            "{} failed to boot here before, so it is not tried again",
            image.version
        ));
    }
    Ok(())
}

fn majors(manifest: &Manifest) -> String {
    manifest
        .supported_majors
        .iter()
        .map(|m| m.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Chooses the newest image `check` allows. A user on a version that lost support moves to
/// the newest supported one, the same as everyone else.
pub fn decide(installed: &InstalledImage, manifest: &Manifest, failed: &[String]) -> Decision {
    let newest = manifest
        .images
        .iter()
        .filter(|image| check(installed, image, manifest, failed).is_ok())
        .max_by_key(|image| (image.android_major, image.build));
    match newest {
        Some(image) => Decision::Apply(image.clone()),
        None if !manifest.supported_majors.contains(&installed.android_major) => {
            Decision::Unsupported(format!(
                "Android {} ({}) is no longer supported (supported: {}) and no image to move \
                 to is available yet",
                installed.android_major,
                installed.version,
                majors(manifest)
            ))
        }
        None => Decision::UpToDate,
    }
}
