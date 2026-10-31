//! Logging to the user's LocalAppData DSA directory.

use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use dsa_host_core::resize_trace::{self, ResizeTrace};

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

/// Returns the default log file path: `%LOCALAPPDATA%\DSA\logs\dsa-wm.log`.
pub fn default_log_file() -> PathBuf {
    default_log_dir().join("dsa-wm.log")
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
            let millis = dur.subsec_millis();
            let secs = total_secs % 60;
            let total_mins = total_secs / 60;
            let mins = total_mins % 60;
            let total_hours = total_mins / 60;
            let hours = total_hours % 24;
            let days = total_hours / 24;
            format!("+{days}d {hours:02}:{mins:02}:{secs:02}.{millis:03} UTC")
        }
        Err(_) => "0 UTC".to_string(),
    }
}

/// Writes `message` to `logger`, or to standard error when there is no log file.
pub fn note(logger: Option<&CompositorLogger>, message: &str) {
    match logger {
        Some(logger) => logger.log(message),
        None => eprintln!("dsa-wm: {message}"),
    }
}

/// The resize trace of this process. Its per-stage lines are on when the environment
/// variable `DSA_RESIZE_TRACE` is set, or the file `%LOCALAPPDATA%\DSA\resize-trace` exists
/// when the process first traces.
fn resize_trace() -> &'static Mutex<ResizeTrace> {
    static TRACE: OnceLock<Mutex<ResizeTrace>> = OnceLock::new();
    TRACE.get_or_init(|| {
        let file = default_log_dir()
            .parent()
            .map(|dsa| dsa.join(resize_trace::DETAIL_FILE).exists())
            .unwrap_or(false);
        let env = std::env::var(resize_trace::DETAIL_ENV).ok();
        Mutex::new(ResizeTrace::new(
            resize_trace::detail_requested(env.as_deref(), file),
            Instant::now(),
        ))
    })
}

/// Whether this process writes the per-stage resize trace lines.
pub fn trace_detailed() -> bool {
    resize_trace()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .detailed()
}

/// Logs `stage` of the resize of `task_id` to `size`, when the per-stage lines are on and
/// the stage has not been logged at that size already.
pub fn trace_stage(
    logger: Option<&CompositorLogger>,
    task_id: i32,
    stage: &'static str,
    size: (u32, u32),
    detail: &str,
) {
    let line = resize_trace()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .stage(
            task_id,
            stage,
            size,
            detail,
            Instant::now(),
            resize_trace::wall_now(),
        );
    if let Some(line) = line {
        note(logger, &line);
    }
}

/// Logs the summary `stage` of the resize of `task_id` to `size`, unless the task's last
/// summary went out moments ago.
pub fn trace_summary(
    logger: Option<&CompositorLogger>,
    task_id: i32,
    stage: &'static str,
    size: (u32, u32),
    detail: &str,
) {
    let line = resize_trace()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .summary(
            task_id,
            stage,
            size,
            detail,
            Instant::now(),
            resize_trace::wall_now(),
        );
    if let Some(line) = line {
        note(logger, &line);
    }
}

/// The task's window is gone: its trace state goes too.
pub fn trace_forget(task_id: i32) {
    resize_trace()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .forget(task_id);
}
