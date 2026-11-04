//! Logging to the user's LocalAppData DSA directory.

use std::fs::{create_dir_all, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant, SystemTime};

use dsa_host_core::input_trace as input_trace_line;
use dsa_host_core::resize_trace::{self, ResizeTrace};

/// Returns the default log folder for the current user: `%LOCALAPPDATA%\DSA\logs`.
pub fn default_log_dir() -> PathBuf {
    crate::paths::UserRoot::current()
        .map(|root| root.logs())
        .unwrap_or_else(|| PathBuf::from("logs"))
}

/// Returns the default log file path: `%LOCALAPPDATA%\DSA\logs\dsa-wm.log`.
pub fn default_log_file() -> PathBuf {
    crate::paths::UserRoot::current()
        .map(|root| root.window_manager_log())
        .unwrap_or_else(|| PathBuf::from("logs").join("dsa-wm.log"))
}

/// Returns the app window host's log file path: `%LOCALAPPDATA%\DSA\logs\dsa-app-window.log`.
pub fn default_app_window_log_file() -> PathBuf {
    crate::paths::UserRoot::current()
        .map(|root| root.app_window_log())
        .unwrap_or_else(|| PathBuf::from("logs").join("dsa-app-window.log"))
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

/// Where this process's input trace lines go.
static INPUT_TRACE_LOG: OnceLock<Arc<CompositorLogger>> = OnceLock::new();

/// Sends this process's input trace lines to `logger`.
pub fn input_trace_to(logger: Option<Arc<CompositorLogger>>) {
    if let Some(logger) = logger {
        let _ = INPUT_TRACE_LOG.set(logger);
    }
}

/// The trace settings this process read from the service: (input trace, resize trace).
static TRACE_SETTINGS: OnceLock<(bool, bool)> = OnceLock::new();

/// Takes the `input_trace` and `resize_trace` settings the service reported, before the first
/// trace line. Without them only the environment variables turn the traces on.
pub fn use_trace_settings(input: bool, resize: bool) {
    let _ = TRACE_SETTINGS.set((input, resize));
}

/// Asks the service on the pipe `pipe_name` for the trace settings and takes them
/// ([`use_trace_settings`]). A service that cannot be asked leaves the traces off.
pub fn read_trace_settings(pipe_name: &str, build: &str) -> Result<(), String> {
    let mut connector = crate::pipe::NamedPipeConnector {
        name: pipe_name.to_string(),
    };
    let mut client = dsa_host_core::host_api::HostApiClient::connect(
        &mut connector,
        build,
        Duration::from_secs(5),
    )
    .map_err(|e| e.to_string())?;
    let report = client.settings().map_err(|e| e.to_string())?;
    use_trace_settings(report.settings.input_trace, report.settings.resize_trace);
    Ok(())
}

/// Whether this process writes the input trace: the environment variable `DSA_INPUT_TRACE` is
/// set, or the `input_trace` setting was on when the process started.
pub fn input_trace_on() -> bool {
    static ON: OnceLock<bool> = OnceLock::new();
    *ON.get_or_init(|| {
        let setting = TRACE_SETTINGS.get().is_some_and(|(input, _)| *input);
        let env = std::env::var(input_trace_line::DETAIL_ENV).ok();
        resize_trace::detail_requested(env.as_deref(), setting)
    })
}

/// The host's monotonic clock: the performance counter, which every process on the computer
/// shares, as time since its origin.
pub fn host_clock() -> Duration {
    use windows_sys::Win32::System::Performance::{
        QueryPerformanceCounter, QueryPerformanceFrequency,
    };
    let (mut now, mut frequency) = (0i64, 0i64);
    // SAFETY: both write one local integer.
    unsafe {
        QueryPerformanceCounter(&mut now);
        QueryPerformanceFrequency(&mut frequency);
    }
    qpc_to_duration(now, frequency)
}

pub use dsa_host_core::compositor::present_watch::qpc_to_duration;

/// The performance counter's ticks a second.
pub fn qpc_frequency() -> i64 {
    use windows_sys::Win32::System::Performance::QueryPerformanceFrequency;
    let mut frequency = 0i64;
    // SAFETY: writes one local integer.
    unsafe {
        QueryPerformanceFrequency(&mut frequency);
    }
    frequency
}

/// [`host_clock`] for the platform-neutral clock pairing.
pub struct PerformanceCounterClock;

impl dsa_host_core::clock_trace::HostClock for PerformanceCounterClock {
    fn now(&self) -> Duration {
        host_clock()
    }
}

/// Writes one input trace line, when the trace is on.
pub fn input_trace(stage: &str, task_id: i32, detail: &str) {
    if !input_trace_on() {
        return;
    }
    static START: OnceLock<Instant> = OnceLock::new();
    let line = input_trace_line::line(
        stage,
        task_id,
        detail,
        START.get_or_init(Instant::now).elapsed(),
        resize_trace::wall_now(),
        host_clock(),
    );
    note(INPUT_TRACE_LOG.get().map(|logger| &**logger), &line);
}

/// The resize trace of this process. Its per-stage lines are on when the environment
/// variable `DSA_RESIZE_TRACE` is set, or the `resize_trace` setting was on when the process
/// started.
fn resize_trace() -> &'static Mutex<ResizeTrace> {
    static TRACE: OnceLock<Mutex<ResizeTrace>> = OnceLock::new();
    TRACE.get_or_init(|| {
        let setting = TRACE_SETTINGS.get().is_some_and(|(_, resize)| *resize);
        let env = std::env::var(resize_trace::DETAIL_ENV).ok();
        Mutex::new(ResizeTrace::new(
            resize_trace::detail_requested(env.as_deref(), setting),
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
