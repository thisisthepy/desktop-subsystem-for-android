//! The user's monitors, for choosing the guest display's size when their Android starts.
//!
//! The service runs in session 0, whose desktop is not the user's: monitor calls made there
//! describe session 0's. So the processes in the user's session record the monitors in a small
//! file in the user's local application data, before they ask for Android and whenever the
//! monitors change, and the service reads it when it starts the VM. The record outlives them
//! all, so an Android started with no window process running still gets the last known size.
//! Without a record the size the image names stays.

use std::io::Read;
use std::path::{Path, PathBuf};

use dsa_host_core::guest_display::{display_for, parse_record, record_text};

/// More than any real record.
const MAX_RECORD_BYTES: u64 = 4096;

/// Where the monitors are recorded, below a user's local application data.
pub fn record_path(local_app_data: &Path) -> PathBuf {
    crate::paths::UserRoot::in_local_app_data(local_app_data).monitors()
}

/// The guest display for the monitors recorded below `local_app_data`, when there is a record
/// with a usable monitor in it.
pub fn recorded_display(local_app_data: &Path) -> Option<(u32, u32)> {
    let file = std::fs::File::open(record_path(local_app_data)).ok()?;
    let mut text = String::new();
    file.take(MAX_RECORD_BYTES).read_to_string(&mut text).ok()?;
    display_for(&parse_record(&text))
}

/// Writes the record whole, so the service never reads half of one.
pub fn write_record(local_app_data: &Path, monitors: &[(u32, u32)]) -> std::io::Result<()> {
    let path = record_path(local_app_data);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("txt.new");
    std::fs::write(&partial, record_text(monitors))?;
    std::fs::rename(&partial, &path)
}

#[cfg(windows)]
pub use session::{current, record_current, system_density, MonitorWatch};

#[cfg(windows)]
mod session {
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::{LPARAM, RECT, TRUE};
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, GetMonitorInfoW, HDC, HMONITOR, MONITORINFO,
    };
    use windows_sys::Win32::UI::HiDpi::{
        GetDpiForSystem, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    };

    use super::write_record;

    /// The density apps draw at on this session's main monitor, as a task's window gives it:
    /// 160 at a scale of 100%, 240 at 150%. A split-APK bundle's density split is chosen for
    /// it.
    pub fn system_density() -> u32 {
        // SAFETY: no pointers. A thread that is not DPI aware would be told 96 whatever the
        // scale, so it asks as a per-monitor aware one, and goes back.
        let dpi = unsafe {
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let dpi = GetDpiForSystem();
            SetThreadDpiAwarenessContext(previous);
            dpi
        };
        dsa_host_core::compositor::window_sizing::density_for_dpi(dpi)
    }

    unsafe extern "system" fn add_monitor(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        list: LPARAM,
    ) -> i32 {
        let list = &mut *(list as *mut Vec<(u32, u32)>);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            let rect = info.rcMonitor;
            list.push((
                (rect.right - rect.left).max(0) as u32,
                (rect.bottom - rect.top).max(0) as u32,
            ));
        }
        TRUE
    }

    /// The monitors of this session in physical pixels, in the order Windows lists them.
    pub fn current() -> Vec<(u32, u32)> {
        let mut monitors: Vec<(u32, u32)> = Vec::new();
        // SAFETY: the callback only runs during the call, while `monitors` lives. A thread
        // that is not per-monitor aware would see a 200% monitor at half its pixels.
        unsafe {
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(add_monitor),
                &mut monitors as *mut Vec<(u32, u32)> as LPARAM,
            );
            SetThreadDpiAwarenessContext(previous);
        }
        monitors
    }

    fn local_app_data() -> Option<PathBuf> {
        crate::paths::UserRoot::current().and_then(|root| root.dir().parent().map(PathBuf::from))
    }

    /// Records this session's monitors for the service.
    pub fn record_current() -> Result<(), String> {
        record(&current())
    }

    /// A session that reports no monitors (no desktop, as over SSH) leaves the last record.
    fn record(monitors: &[(u32, u32)]) -> Result<(), String> {
        if monitors.is_empty() {
            return Ok(());
        }
        let dir = local_app_data().ok_or("this user's local application data folder is unknown")?;
        write_record(&dir, monitors).map_err(|e| format!("recording the monitors: {e}"))
    }

    /// Keeps the record up to date from a loop that runs anyway.
    #[derive(Default)]
    pub struct MonitorWatch {
        last: Option<Vec<(u32, u32)>>,
    }

    impl MonitorWatch {
        /// Records the monitors if they changed since the last call. A failure is returned
        /// once, not on every call.
        pub fn check(&mut self) -> Result<(), String> {
            let now = current();
            if self.last.as_ref() == Some(&now) {
                return Ok(());
            }
            let result = record(&now);
            self.last = Some(now);
            result
        }
    }
}
