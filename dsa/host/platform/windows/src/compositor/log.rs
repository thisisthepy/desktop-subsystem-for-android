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

/// Where the graphics renderer's lines go.
static RENDERER_LOG: OnceLock<Arc<CompositorLogger>> = OnceLock::new();

/// Sends the graphics renderer's warnings, errors and fatal errors to `logger`.
pub fn renderer_log_to(logger: Option<Arc<CompositorLogger>>) {
    if let Some(logger) = logger {
        let _ = RENDERER_LOG.set(logger);
    }
}

/// How one of the graphics renderer's lines reads in the log: `severity` is the renderer's
/// `W`, `E` or `F`. Other severities are not written.
pub fn renderer_line(severity: u8, message: &str) -> Option<String> {
    let message = message.trim_end();
    match severity {
        b'F' => Some(format!(
            "graphics renderer fatal error (the renderer ends what it was doing): {message}"
        )),
        b'E' => Some(format!("graphics renderer error: {message}")),
        b'W' => Some(format!("graphics renderer warning: {message}")),
        _ => None,
    }
}

/// Where the graphics renderer's lines go instead of a log file, when they are set to: in
/// the renderer process, the window manager's log over its connection.
pub type RendererLines = Box<dyn Fn(u8, &str) + Send + Sync>;
static RENDERER_LINES: OnceLock<RendererLines> = OnceLock::new();

/// Sends the graphics renderer's lines to `forward` (severity and message, as gfxstream gave
/// them) instead of a log file.
pub fn renderer_log_through(forward: RendererLines) {
    let _ = RENDERER_LINES.set(forward);
}

/// Writes one of the graphics renderer's lines, from any of its threads.
pub fn renderer_log(severity: u8, message: &str) {
    if let Some(forward) = RENDERER_LINES.get() {
        if matches!(severity, b'W' | b'E' | b'F') {
            forward(severity, message);
        }
        return;
    }
    if let Some(line) = renderer_line(severity, message) {
        note(RENDERER_LOG.get().map(|logger| &**logger), &line);
    }
}

/// How a panic reads in the log: the thread, where it panicked, and what it said.
pub fn panic_line(thread: Option<&str>, location: Option<(&str, u32)>, message: &str) -> String {
    let thread = thread.unwrap_or("unnamed");
    match location {
        Some((file, line)) => format!("panic in thread '{thread}' at {file}:{line}: {message}"),
        None => format!("panic in thread '{thread}': {message}"),
    }
}

/// Writes every panic to `logger` before Rust's own report, so the log says why a thread, or
/// the process, ended even though the process has no console.
pub fn log_panics_to(logger: Option<Arc<CompositorLogger>>) {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<&str>()
            .map(|text| text.to_string())
            .or_else(|| payload.downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_string());
        let thread = std::thread::current();
        let line = panic_line(
            thread.name(),
            info.location().map(|at| (at.file(), at.line())),
            &message,
        );
        note(logger.as_deref(), &line);
        previous(info);
    }));
}

/// What a Windows exception code means, for the line a crash writes.
pub fn exception_name(code: u32) -> &'static str {
    match code {
        0xC000_0005 => "access violation",
        0xC000_00FD => "stack overflow",
        0xC000_0094 => "integer division by zero",
        0xC000_0095 => "integer overflow",
        0xC000_001D => "illegal instruction",
        0xC000_0096 => "privileged instruction",
        0xC000_0374 => "heap corruption",
        0xC000_0409 => "fast fail (the program stopped itself)",
        0xC000_0017 => "no memory",
        0x8000_0003 => "breakpoint",
        _ => "unknown exception",
    }
}

/// How a crash that no handler took reads in the log: the thread, the exception and where it
/// happened. A native crash is not a panic, so the panic hook never sees it.
pub fn crash_line(thread: Option<&str>, code: u32, address: usize) -> String {
    crash_line_with(thread, code, address, None, None)
}

/// [`crash_line`] with the module the address is in (its file name and the address's offset
/// from the module's base, which names the function in the module's symbols) and the minidump
/// that was written.
pub fn crash_line_with(
    thread: Option<&str>,
    code: u32,
    address: usize,
    module: Option<(&str, usize)>,
    dump: Option<&Path>,
) -> String {
    let thread = thread.unwrap_or("unnamed");
    let place = match module {
        Some((name, offset)) => format!("{address:#x} (in {name} at offset {offset:#x})"),
        None => format!("{address:#x}"),
    };
    let dump = match dump {
        Some(path) => format!("; a minidump is in {}", path.display()),
        None => String::new(),
    };
    format!(
        "crash in thread '{thread}': {} (code {code:#010x}) at address {place}{dump}; the process ends",
        exception_name(code)
    )
}

/// How many minidumps of crashes the log folder keeps.
pub const CRASH_DUMPS_KEPT: usize = 5;

/// The file name of the minidump a crash of `process` at `unix_secs` writes:
/// `<process>-<UTC time>.dmp`, such as `dsa-wm-20261116T055552Z.dmp`.
pub fn crash_dump_name(process: &str, unix_secs: u64) -> String {
    let days = (unix_secs / 86_400) as i64;
    let rest = unix_secs % 86_400;
    // Days since 1970-01-01 to a civil date (Howard Hinnant's algorithm).
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_part = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_part + 2) / 5 + 1;
    let month = if month_part < 10 {
        month_part + 3
    } else {
        month_part - 9
    };
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    format!(
        "{process}-{year:04}{month:02}{day:02}T{:02}{:02}{:02}Z.dmp",
        rest / 3_600,
        rest % 3_600 / 60,
        rest % 60
    )
}

/// The minidumps among `names` to delete so that at most `keep` remain: the oldest by the UTC
/// time in their names, whichever process wrote them. Other files are never named.
pub fn crash_dumps_to_delete(names: &[String], keep: usize) -> Vec<String> {
    let mut dumps: Vec<(&str, &String)> = names
        .iter()
        .filter_map(|name| {
            let stem = name.strip_suffix(".dmp")?;
            let (_, time) = stem.rsplit_once('-')?;
            Some((time, name))
        })
        .collect();
    dumps.sort();
    let excess = dumps.len().saturating_sub(keep);
    dumps
        .into_iter()
        .take(excess)
        .map(|(_, name)| name.clone())
        .collect()
}

#[cfg(windows)]
static CRASH_LOG: OnceLock<Arc<CompositorLogger>> = OnceLock::new();

/// Writes a minidump of the crashed process into the `crash` folder beside the log, after
/// deleting the oldest dumps so that at most [`CRASH_DUMPS_KEPT`] remain. Returns where it went.
///
/// # Safety
///
/// `info` is the pointer Windows gave the unhandled exception filter.
#[cfg(windows)]
unsafe fn write_minidump(
    logger: &CompositorLogger,
    info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> Option<PathBuf> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{CloseHandle, GENERIC_WRITE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, CREATE_ALWAYS, FILE_ATTRIBUTE_NORMAL,
    };
    use windows_sys::Win32::System::Diagnostics::Debug::{
        MiniDumpWithIndirectlyReferencedMemory, MiniDumpWithThreadInfo,
        MiniDumpWithUnloadedModules, MiniDumpWriteDump, MINIDUMP_EXCEPTION_INFORMATION,
    };
    use windows_sys::Win32::System::Threading::{
        GetCurrentProcess, GetCurrentProcessId, GetCurrentThreadId,
    };
    let dir = logger.path().parent()?.join("crash");
    std::fs::create_dir_all(&dir).ok()?;
    let names: Vec<String> = std::fs::read_dir(&dir)
        .ok()?
        .filter_map(|entry| entry.ok()?.file_name().into_string().ok())
        .collect();
    // One is about to be written.
    for name in crash_dumps_to_delete(&names, CRASH_DUMPS_KEPT - 1) {
        let _ = std::fs::remove_file(dir.join(name));
    }
    let process = std::env::current_exe()
        .ok()
        .and_then(|exe| Some(exe.file_stem()?.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "dsa".into());
    let secs = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .map(|since| since.as_secs())
        .unwrap_or(0);
    let path = dir.join(crash_dump_name(&process, secs));
    let wide: Vec<u16> = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let file = CreateFileW(
        wide.as_ptr(),
        GENERIC_WRITE,
        0,
        std::ptr::null(),
        CREATE_ALWAYS,
        FILE_ATTRIBUTE_NORMAL,
        std::ptr::null_mut(),
    );
    if file == INVALID_HANDLE_VALUE {
        return None;
    }
    let exception = MINIDUMP_EXCEPTION_INFORMATION {
        ThreadId: GetCurrentThreadId(),
        ExceptionPointers: info as *mut _,
        ClientPointers: 0,
    };
    let written = MiniDumpWriteDump(
        GetCurrentProcess(),
        GetCurrentProcessId(),
        file,
        MiniDumpWithThreadInfo
            | MiniDumpWithIndirectlyReferencedMemory
            | MiniDumpWithUnloadedModules,
        &exception,
        std::ptr::null(),
        std::ptr::null(),
    );
    CloseHandle(file);
    if written == 0 {
        let _ = std::fs::remove_file(&path);
        return None;
    }
    Some(path)
}

/// The module `address` is in, as its file name and the address's offset from its base.
///
/// # Safety
///
/// Plain calls with locals; `address` is only looked up, never read.
#[cfg(windows)]
unsafe fn module_of(address: usize) -> Option<(String, usize)> {
    use windows_sys::Win32::System::LibraryLoader::{
        GetModuleFileNameW, GetModuleHandleExW, GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS,
        GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
    };
    let mut module = std::ptr::null_mut();
    let found = GetModuleHandleExW(
        GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
        address as *const u16,
        &mut module,
    );
    if found == 0 || module.is_null() {
        return None;
    }
    let mut buffer = [0u16; 520];
    let length = GetModuleFileNameW(module, buffer.as_mut_ptr(), buffer.len() as u32) as usize;
    if length == 0 {
        return None;
    }
    let path = String::from_utf16_lossy(&buffer[..length.min(buffer.len())]);
    let name = path.rsplit(['\\', '/']).next().unwrap_or(&path).to_string();
    Some((name, address.saturating_sub(module as usize)))
}

#[cfg(windows)]
unsafe extern "system" fn write_crash(
    info: *const windows_sys::Win32::System::Diagnostics::Debug::EXCEPTION_POINTERS,
) -> i32 {
    // SAFETY: Windows passes a valid pointer; the record is only read.
    if let Some(record) = info.as_ref().and_then(|info| info.ExceptionRecord.as_ref()) {
        let thread = std::thread::current();
        let address = record.ExceptionAddress as usize;
        let module = module_of(address);
        let dump = CRASH_LOG
            .get()
            .and_then(|logger| write_minidump(logger, info));
        let line = crash_line_with(
            thread.name(),
            record.ExceptionCode as u32,
            address,
            module
                .as_ref()
                .map(|(name, offset)| (name.as_str(), *offset)),
            dump.as_deref(),
        );
        note(CRASH_LOG.get().map(|logger| &**logger), &line);
    }
    // EXCEPTION_CONTINUE_SEARCH: Windows still reports the crash as it would have.
    0
}

/// Writes a crash that kills the process (an access violation in a driver, a stack overflow, a
/// division by zero) to `logger` before the process ends, with the module and offset it
/// happened at, and a minidump in the `crash` folder beside the log (the newest
/// [`CRASH_DUMPS_KEPT`] are kept). Without it such a death leaves no line at all, because it
/// never reaches the panic hook.
pub fn log_crashes_to(logger: Option<Arc<CompositorLogger>>) {
    #[cfg(windows)]
    if let Some(logger) = logger {
        if CRASH_LOG.set(logger).is_ok() {
            // SAFETY: installs a filter that only reads the exception it is given.
            unsafe {
                windows_sys::Win32::System::Diagnostics::Debug::SetUnhandledExceptionFilter(Some(
                    write_crash,
                ));
            }
        }
    }
    #[cfg(not(windows))]
    let _ = logger;
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

/// Logs `stage` of `task_id`'s window placed at `pos` with `size`, when the per-stage
/// lines are on.
pub fn trace_place(
    logger: Option<&CompositorLogger>,
    task_id: i32,
    stage: &'static str,
    pos: (i32, i32),
    size: (u32, u32),
    detail: &str,
) {
    let line = resize_trace()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .place(
            task_id,
            stage,
            pos,
            size,
            detail,
            Instant::now(),
            resize_trace::wall_now(),
        );
    if let Some(line) = line {
        note(logger, &line);
    }
}

/// Logs `task_id`'s window dragged to `pos`, a few times a second at most.
pub fn trace_moving(
    logger: Option<&CompositorLogger>,
    task_id: i32,
    pos: (i32, i32),
    size: (u32, u32),
) {
    let line = resize_trace()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .moving(task_id, pos, size, Instant::now(), resize_trace::wall_now());
    if let Some(line) = line {
        note(logger, &line);
    }
}

/// `task_id`'s drag has ended: logs its last move, if that was left out.
pub fn trace_move_ended(logger: Option<&CompositorLogger>, task_id: i32) {
    let line = resize_trace()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .move_ended(task_id);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr5_a_crash_line_names_the_module_and_offset_and_the_dump() {
        let line = crash_line_with(
            Some("dsa-wm-host"),
            0xC000_0005,
            0x7ffb_0cf7_10a3,
            Some(("igd10umt64xe.dll", 0xc10a3)),
            Some(Path::new("crash/dsa-wm-20261116T055552Z.dmp")),
        );
        assert_eq!(
            line,
            "crash in thread 'dsa-wm-host': access violation (code 0xc0000005) at address \
             0x7ffb0cf710a3 (in igd10umt64xe.dll at offset 0xc10a3); a minidump is in \
             crash/dsa-wm-20261116T055552Z.dmp; the process ends"
        );
    }

    #[test]
    fn nfr5_a_crash_dump_is_named_after_the_process_and_the_utc_time() {
        assert_eq!(
            crash_dump_name("dsa-wm", 1_794_808_552),
            "dsa-wm-20261116T055552Z.dmp"
        );
        assert_eq!(
            crash_dump_name("dsa-app-window", 0),
            "dsa-app-window-19700101T000000Z.dmp"
        );
        // A leap day.
        assert_eq!(
            crash_dump_name("x", 1_709_208_000),
            "x-20240229T120000Z.dmp"
        );
    }

    #[test]
    fn nfr5_only_the_newest_crash_dumps_are_kept_whichever_process_wrote_them() {
        let names: Vec<String> = [
            "dsa-wm-20261101T000000Z.dmp",
            "dsa-app-window-20261102T000000Z.dmp",
            "dsa-wm-20261103T000000Z.dmp",
            "notes.txt",
            "dsa-wm-20261104T000000Z.dmp",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            crash_dumps_to_delete(&names, 2),
            vec![
                "dsa-wm-20261101T000000Z.dmp".to_string(),
                "dsa-app-window-20261102T000000Z.dmp".to_string()
            ]
        );
        assert!(crash_dumps_to_delete(&names, 5).is_empty());
    }

    #[test]
    fn nfr5_a_fatal_renderer_error_is_logged_with_its_reason() {
        assert_eq!(
            renderer_line(
                b'F',
                "VulkanStream.cpp:98: VulkanStream can't allocate UINT32_MAX bytes\n"
            )
            .as_deref(),
            Some(
                "graphics renderer fatal error (the renderer ends what it was doing): \
                 VulkanStream.cpp:98: VulkanStream can't allocate UINT32_MAX bytes"
            )
        );
        assert_eq!(
            renderer_line(b'E', "A Vulkan device was lost.").as_deref(),
            Some("graphics renderer error: A Vulkan device was lost.")
        );
        assert_eq!(
            renderer_line(b'W', "slow").as_deref(),
            Some("graphics renderer warning: slow")
        );
        assert_eq!(renderer_line(b'I', "Destroyed VkDevice"), None);
        assert_eq!(renderer_line(b'V', "verbose"), None);
    }

    #[test]
    fn nfr5_a_native_crash_is_logged_with_its_thread_exception_and_address() {
        assert_eq!(
            crash_line(Some("dsa-wm-hosts"), 0xC000_0005, 0x7ff6_1234),
            "crash in thread 'dsa-wm-hosts': access violation (code 0xc0000005) at address \
             0x7ff61234; the process ends"
        );
        assert!(crash_line(None, 0xC000_00FD, 0).contains("stack overflow"));
        assert_eq!(exception_name(0x1234), "unknown exception");
    }

    #[test]
    fn nfr5_a_panic_is_logged_with_its_thread_place_and_message() {
        assert_eq!(
            panic_line(
                Some("dsa-wm-hosts"),
                Some(("src/compositor/hosts.rs", 42)),
                "boom"
            ),
            "panic in thread 'dsa-wm-hosts' at src/compositor/hosts.rs:42: boom"
        );
        assert_eq!(
            panic_line(None, None, "boom"),
            "panic in thread 'unnamed': boom"
        );
    }
}
