//! Erasing a user's Android data: the apps they installed, their accounts and every app's
//! data.
//!
//! A reset stops the user's Android, waits until it has stopped, and has the platform put a
//! fresh data disk in place of theirs ([`UserDataReset`]). The fresh disk comes from the
//! Android image, as a new user's does, so the next start boots Android as it was first
//! installed. The system image is not touched, and neither are DSA's settings, which the
//! service keeps in its own folder.
//!
//! [`TemplateDataDisk`] is the part every platform shares: the old disk is moved aside, a new
//! one is made, and only then is the old one removed. A new disk that cannot be made puts the
//! old one back, so a failed reset loses nothing.

use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use crate::host_api::UserAndroid;
use crate::session::SessionState;

/// How long a reset waits for the user's Android to stop.
pub const RESET_STOP_WAIT: Duration = Duration::from_secs(120);

/// How long a client waits for the answer to a reset: the stop, and making the new disk.
pub const RESET_ANSWER_WAIT: Duration = Duration::from_secs(600);

/// How often a reset looks whether Android has stopped.
const POLL: Duration = Duration::from_millis(100);

/// Puts a fresh data disk in place of a user's, while their Android is stopped.
pub trait UserDataReset: Send + Sync {
    /// Replaces the data disk of `user` with a fresh one. On an error the old disk is still
    /// there.
    fn replace_disk(&self, user: &str) -> Result<(), String>;
}

/// Stops `android`, waits at most `wait` for it to stop, and has `reset` replace the data
/// disk of `user`.
pub fn reset_user_data(
    android: &dyn UserAndroid,
    reset: &dyn UserDataReset,
    user: &str,
    wait: Duration,
) -> Result<(), String> {
    if android.state() != SessionState::Stopped {
        android.stop();
        let deadline = Instant::now() + wait;
        loop {
            let state = android.state();
            if state == SessionState::Stopped {
                break;
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "Android did not stop within {} s (it is {state}), so its data was left as \
                     it is; stop it and try again",
                    wait.as_secs()
                ));
            }
            thread::sleep(POLL);
        }
    }
    reset.replace_disk(user)
}

/// Finds the data disk of a user.
pub type DiskOf = Box<dyn Fn(&str) -> Result<PathBuf, String> + Send + Sync>;

/// Makes a fresh data disk at the path it is given, from the Android image.
pub type MakeFreshDisk = Box<dyn Fn(&Path) -> Result<(), String> + Send + Sync>;

/// A [`UserDataReset`] that moves the old disk aside, makes a new one with `make_fresh`, and
/// removes the old one. Files kept beside the disk with the same name and another extension
/// ([`TemplateDataDisk::with_beside`], such as the image version the disk holds) go with it.
pub struct TemplateDataDisk {
    disk_of: DiskOf,
    make_fresh: MakeFreshDisk,
    beside: Vec<String>,
}

impl TemplateDataDisk {
    pub fn new(disk_of: DiskOf, make_fresh: MakeFreshDisk) -> TemplateDataDisk {
        TemplateDataDisk {
            disk_of,
            make_fresh,
            beside: Vec::new(),
        }
    }

    /// Also moves the file beside the disk with `extension` in place of its own: it describes
    /// the old disk, so it goes with it.
    pub fn with_beside(mut self, extension: &str) -> TemplateDataDisk {
        self.beside.push(extension.to_string());
        self
    }
}

/// Removes a user's data disk (and `version_file`, the image version recorded beside it)
/// when it holds data of a newer Android major than `image_major`. That happens once a user
/// moved to an older major and agreed to erase the data: Android cannot read data a newer
/// major wrote. The platform then makes a fresh disk from the image, as for a new user.
/// Returns whether the disk was removed. Call it only while the user's Android is stopped.
pub fn erase_if_newer(disk: &Path, version_file: &Path, image_major: u32) -> Result<bool, String> {
    let held = std::fs::read_to_string(version_file)
        .ok()
        .and_then(|version| crate::update::versions::major_of_version(&version));
    match held {
        Some(major) if major > image_major && disk.exists() => {
            std::fs::remove_file(disk).map_err(|e| {
                format!(
                    "erasing {}, which holds Android {major} data: {e}",
                    disk.display()
                )
            })?;
            let _ = std::fs::remove_file(version_file);
            eprintln!(
                "{} held Android {major} data, which Android {image_major} cannot read; it was                  erased as the user agreed when they chose Android {image_major}",
                disk.display()
            );
            Ok(true)
        }
        _ => Ok(false),
    }
}

/// Where a file waits while its replacement is made.
fn aside(path: &Path) -> PathBuf {
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    name.push(".before-reset");
    path.with_file_name(name)
}

fn rename(from: &Path, to: &Path) -> Result<(), String> {
    std::fs::rename(from, to)
        .map_err(|e| format!("moving {} to {}: {e}", from.display(), to.display()))
}

impl UserDataReset for TemplateDataDisk {
    fn replace_disk(&self, user: &str) -> Result<(), String> {
        let disk = (self.disk_of)(user)?;
        let mut files = vec![disk.clone()];
        files.extend(self.beside.iter().map(|ext| disk.with_extension(ext)));
        // What an earlier reset left behind is not the user's current data.
        for file in &files {
            let _ = std::fs::remove_file(aside(file));
        }
        let mut moved: Vec<&PathBuf> = Vec::new();
        for file in &files {
            if file.exists() {
                if let Err(why) = rename(file, &aside(file)) {
                    for back in moved {
                        let _ = rename(&aside(back), back);
                    }
                    return Err(why);
                }
                moved.push(file);
            }
        }
        if let Err(why) = (self.make_fresh)(&disk) {
            // Whatever half of a new disk was made goes, and the old one comes back.
            for file in &files {
                let _ = std::fs::remove_file(file);
            }
            for back in moved {
                let _ = rename(&aside(back), back);
            }
            return Err(format!(
                "a new disk could not be made, so the old one was kept: {why}"
            ));
        }
        for file in moved {
            if let Err(e) = std::fs::remove_file(aside(file)) {
                // The new disk is in place; the old one only takes room until the next reset.
                eprintln!(
                    "data reset: the old {} was not removed: {e}",
                    aside(file).display()
                );
            }
        }
        Ok(())
    }
}
