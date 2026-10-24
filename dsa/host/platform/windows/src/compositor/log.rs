//! Logging to the user's LocalAppData DSA directory.

use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::SystemTime;

/// Returns the default log folder for the current user: `%LOCALAPPDATA%\DSA\logs`.
pub fn default_log_dir() -> PathBuf {
    if let Some(local_app_data) = std::env::var_os("LOCALAPPDATA") {
        PathBuf::from(local_app_data).join("DSA").join("logs")
    } else if let Some(user_profile) = std::env::var_os("USERPROFILE") {
        PathBuf::from(user_profile)
            .join("AppData")
            .join("Local")
            .join("DSA")
            .join("logs")
    } else {
        PathBuf::from("logs")
    }
}

/// Returns the default log file path: `%LOCALAPPDATA%\DSA\logs\dsa-compositor.log`.
pub fn default_log_file() -> PathBuf {
    default_log_dir().join("dsa-compositor.log")
}

/// A thread-safe file logger.
pub struct CompositorLogger {
    file: Mutex<Option<File>>,
    path: PathBuf,
}

impl CompositorLogger {
    /// Opens or creates the log file at `path`.
    pub fn open(path: impl AsRef<Path>) -> Result<CompositorLogger, String> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            create_dir_all(parent)
                .map_err(|e| format!("could not create log directory {}: {e}", parent.display()))?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|e| format!("could not open log file {}: {e}", path.display()))?;
        Ok(CompositorLogger {
            file: Mutex::new(Some(file)),
            path,
        })
    }

    /// Logs a formatted message with a timestamp.
    pub fn log(&self, message: &str) {
        let timestamp = humantime_now();
        let formatted = format!("[{timestamp}] {message}\n");
        if let Ok(mut lock) = self.file.lock() {
            if let Some(file) = lock.as_mut() {
                let _ = file.write_all(formatted.as_bytes());
                let _ = file.flush();
            }
        }
    }

    /// The path to the active log file.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Simple ISO 8601-like timestamp formatter without external time dependencies.
fn humantime_now() -> String {
    let now = SystemTime::now();
    match now.duration_since(SystemTime::UNIX_EPOCH) {
        Ok(dur) => {
            let total_secs = dur.as_secs();
            let secs = total_secs % 60;
            let total_mins = total_secs / 60;
            let mins = total_mins % 60;
            let total_hours = total_mins / 60;
            let hours = total_hours % 24;
            let days = total_hours / 24;
            format!("+{days}d {hours:02}:{mins:02}:{secs:02} UTC")
        }
        Err(_) => "0 UTC".to_string(),
    }
}
