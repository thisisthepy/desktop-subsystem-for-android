//! The user's monitors, for choosing the guest display's size when their Android starts.
//!
//! The service runs in session 0, whose desktop is not the user's: monitor calls made there
//! describe session 0's. So the processes in the user's session record the monitors in a small
//! file in the user's local application data, before they ask for Android and whenever the
//! monitors change, and the service reads it when it starts the VM. Each monitor is recorded
//! with its size in physical pixels, its DPI and whether it is the main one, so the display is
//! sized in guest pixels: a task drawn at the main monitor's density on a monitor of a lower
//! scale needs more of them than the monitor has pixels. The record outlives them
//! all, so an Android started with no window process running still gets the last known size.
//! Without a record the size the image names stays.
//!
//! Beside it they record the refresh rate of the fastest monitor, which the service reads
//! every few seconds while Android runs ([`recorded_refresh`]): the guest's display refreshes
//! at that rate, or at 60 Hz while energy saver is on. Without that record the guest keeps the
//! rate it boots with.
//!
//! And they record the main monitor's DPI, which the service reads when it starts the VM
//! ([`recorded_density`]): Android starts at the density a window on that monitor gives its
//! task, so the apps that open there are never asked for another one right after they start.
//! Without that record Android starts at the density of a 100% monitor.

use std::io::Read;
use std::path::{Path, PathBuf};

use dsa_host_core::guest_display::{
    boot_density, density_record_text, display_for, parse_density_record, parse_record,
    record_text, Monitor,
};
use dsa_host_core::guest_refresh;

/// More than any real record.
const MAX_RECORD_BYTES: u64 = 4096;

/// Opens the plain record file `path` below `local_app_data`, refusing links and hard links
/// in the user's folder.
fn open_record(local_app_data: &Path, path: &Path) -> Option<std::fs::File> {
    #[cfg(windows)]
    {
        let root = crate::paths::UserRoot::in_local_app_data(local_app_data);
        if std::fs::symlink_metadata(root.dir()).is_ok() {
            crate::safe_fs::guard_files(root.dir(), &[path]).ok()?;
        }
    }
    #[cfg(not(windows))]
    let _ = local_app_data;
    std::fs::File::open(path).ok()
}

/// Where the monitors are recorded, below a user's local application data.
pub fn record_path(local_app_data: &Path) -> PathBuf {
    crate::paths::UserRoot::in_local_app_data(local_app_data).monitors()
}

/// The guest display for the monitors recorded below `local_app_data`, when there is a record
/// with a usable monitor in it.
pub fn recorded_display(local_app_data: &Path) -> Option<(u32, u32)> {
    let file = open_record(local_app_data, &record_path(local_app_data))?;
    let mut text = String::new();
    file.take(MAX_RECORD_BYTES).read_to_string(&mut text).ok()?;
    display_for(&parse_record(&text))
}

/// Writes the record whole, so the service never reads half of one.
pub fn write_record(local_app_data: &Path, monitors: &[Monitor]) -> std::io::Result<()> {
    let path = record_path(local_app_data);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("txt.new");
    std::fs::write(&partial, record_text(monitors))?;
    std::fs::rename(&partial, &path)
}

/// Where the fastest monitor's refresh rate is recorded, below a user's local application
/// data.
pub fn refresh_record_path(local_app_data: &Path) -> PathBuf {
    crate::paths::UserRoot::in_local_app_data(local_app_data).monitor_refresh()
}

/// The refresh rate of the fastest monitor recorded below `local_app_data`, in hertz, when
/// there is a record with a rate in it.
pub fn recorded_refresh(local_app_data: &Path) -> Option<u32> {
    let file = open_record(local_app_data, &refresh_record_path(local_app_data))?;
    let mut text = String::new();
    file.take(MAX_RECORD_BYTES).read_to_string(&mut text).ok()?;
    guest_refresh::parse_record(&text)
}

/// Writes the refresh record whole, so the service never reads half of one.
pub fn write_refresh_record(local_app_data: &Path, hz: u32) -> std::io::Result<()> {
    let path = refresh_record_path(local_app_data);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("txt.new");
    std::fs::write(&partial, guest_refresh::record_text(hz))?;
    std::fs::rename(&partial, &path)
}

/// Where the main monitor's DPI is recorded, below a user's local application data.
pub fn density_record_path(local_app_data: &Path) -> PathBuf {
    crate::paths::UserRoot::in_local_app_data(local_app_data).monitor_density()
}

/// The density Android starts at for the main monitor recorded below `local_app_data`: that
/// monitor's, or a 100% monitor's when there is no record.
pub fn recorded_density(local_app_data: &Path) -> u32 {
    let dpi = open_record(local_app_data, &density_record_path(local_app_data)).and_then(|file| {
        let mut text = String::new();
        file.take(MAX_RECORD_BYTES).read_to_string(&mut text).ok()?;
        parse_density_record(&text)
    });
    boot_density(dpi)
}

/// Writes the main monitor's DPI whole, so the service never reads half of a record.
pub fn write_density_record(local_app_data: &Path, dpi: u32) -> std::io::Result<()> {
    let path = density_record_path(local_app_data);
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let partial = path.with_extension("txt.new");
    std::fs::write(&partial, density_record_text(dpi))?;
    std::fs::rename(&partial, &path)
}

/// A monitor's rate from `DEVMODEW.dmDisplayFrequency`, in hertz: 0 and 1 mean the hardware's
/// default rate, which Windows does not name, so they are 0 (unknown).
pub fn frequency_hz(display_frequency: u32) -> u32 {
    if display_frequency <= 1 {
        0
    } else {
        display_frequency
    }
}

#[cfg(windows)]
pub use session::{
    current, current_refresh, main_monitor_dpi, record_current, system_density, MonitorWatch,
};

#[cfg(windows)]
mod session {
    use std::path::PathBuf;

    use windows_sys::Win32::Foundation::{LPARAM, POINT, RECT, TRUE};
    use windows_sys::Win32::Graphics::Gdi::{
        EnumDisplayMonitors, EnumDisplaySettingsW, GetMonitorInfoW, MonitorFromPoint, DEVMODEW,
        ENUM_CURRENT_SETTINGS, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW,
        MONITOR_DEFAULTTOPRIMARY,
    };
    use windows_sys::Win32::UI::HiDpi::{
        GetDpiForMonitor, GetDpiForSystem, SetThreadDpiAwarenessContext,
        DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI,
    };
    use windows_sys::Win32::UI::WindowsAndMessaging::MONITORINFOF_PRIMARY;

    use super::{frequency_hz, write_density_record, write_record, write_refresh_record, Monitor};

    /// The DPI of this session's main monitor (96 is 100%) as it is now. Windows' system DPI
    /// is the main monitor's as of sign-in, so it stands in only when the monitor's own cannot
    /// be read.
    pub fn main_monitor_dpi() -> u32 {
        // SAFETY: plain calls; the out pointers are locals. A thread that is not DPI aware
        // would be told 96 whatever the scale, so it asks as a per-monitor aware one, and goes
        // back.
        unsafe {
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let primary = MonitorFromPoint(POINT { x: 0, y: 0 }, MONITOR_DEFAULTTOPRIMARY);
            let (mut x, mut y) = (0u32, 0u32);
            let dpi = if !primary.is_null()
                && GetDpiForMonitor(primary, MDT_EFFECTIVE_DPI, &mut x, &mut y) == 0
                && x > 0
            {
                x
            } else {
                GetDpiForSystem()
            };
            SetThreadDpiAwarenessContext(previous);
            dpi
        }
    }

    /// The density apps draw at on this session's main monitor, as a task's window gives it:
    /// 130 at a scale of 100%, 196 at 150%. Android starts at it, and a split-APK bundle's
    /// density split is chosen for it.
    pub fn system_density() -> u32 {
        dsa_host_core::guest_display::boot_density(Some(main_monitor_dpi()))
    }

    unsafe extern "system" fn add_monitor(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        list: LPARAM,
    ) -> i32 {
        let list = &mut *(list as *mut Vec<Monitor>);
        let mut info: MONITORINFO = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
        if GetMonitorInfoW(monitor, &mut info) != 0 {
            let rect = info.rcMonitor;
            let (mut x, mut y) = (0u32, 0u32);
            let dpi = (GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) == 0 && x > 0)
                .then_some(x);
            list.push(Monitor {
                width: (rect.right - rect.left).max(0) as u32,
                height: (rect.bottom - rect.top).max(0) as u32,
                dpi,
                main: info.dwFlags & MONITORINFOF_PRIMARY != 0,
            });
        }
        TRUE
    }

    /// The monitors of this session in physical pixels, with their DPI and which is the main
    /// one, in the order Windows lists them.
    pub fn current() -> Vec<Monitor> {
        let mut monitors: Vec<Monitor> = Vec::new();
        // SAFETY: the callback only runs during the call, while `monitors` lives. A thread
        // that is not per-monitor aware would see a 200% monitor at half its pixels, and
        // every monitor at 96 DPI.
        unsafe {
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(add_monitor),
                &mut monitors as *mut Vec<Monitor> as LPARAM,
            );
            SetThreadDpiAwarenessContext(previous);
        }
        monitors
    }

    unsafe extern "system" fn add_refresh(
        monitor: HMONITOR,
        _: HDC,
        _: *mut RECT,
        list: LPARAM,
    ) -> i32 {
        let list = &mut *(list as *mut Vec<u32>);
        let mut info: MONITORINFOEXW = std::mem::zeroed();
        info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
        if GetMonitorInfoW(
            monitor,
            &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
        ) != 0
        {
            let mut mode: DEVMODEW = std::mem::zeroed();
            mode.dmSize = std::mem::size_of::<DEVMODEW>() as u16;
            let hz =
                if EnumDisplaySettingsW(info.szDevice.as_ptr(), ENUM_CURRENT_SETTINGS, &mut mode)
                    != 0
                {
                    frequency_hz(mode.dmDisplayFrequency)
                } else {
                    0
                };
            list.push(hz);
        }
        TRUE
    }

    /// The refresh rate of this session's fastest monitor, in hertz, or `None` when Windows
    /// names no monitor's rate.
    pub fn current_refresh() -> Option<u32> {
        let mut rates: Vec<u32> = Vec::new();
        // SAFETY: the callback only runs during the call, while `rates` lives.
        unsafe {
            EnumDisplayMonitors(
                std::ptr::null_mut(),
                std::ptr::null(),
                Some(add_refresh),
                &mut rates as *mut Vec<u32> as LPARAM,
            );
        }
        dsa_host_core::guest_refresh::fastest(rates)
    }

    fn local_app_data() -> Option<PathBuf> {
        crate::paths::UserRoot::current().and_then(|root| root.dir().parent().map(PathBuf::from))
    }

    /// Records this session's monitors for the service.
    pub fn record_current() -> Result<(), String> {
        record(&current(), current_refresh(), main_monitor_dpi())
    }

    /// A session that reports no monitors (no desktop, as over SSH) leaves the last record,
    /// and one that names no monitor's rate leaves the last rate.
    fn record(monitors: &[Monitor], refresh: Option<u32>, dpi: u32) -> Result<(), String> {
        if monitors.is_empty() {
            return Ok(());
        }
        let dir = local_app_data().ok_or("this user's local application data folder is unknown")?;
        write_record(&dir, monitors).map_err(|e| format!("recording the monitors: {e}"))?;
        write_density_record(&dir, dpi)
            .map_err(|e| format!("recording the main monitor's scale: {e}"))?;
        match refresh {
            Some(hz) => write_refresh_record(&dir, hz)
                .map_err(|e| format!("recording the monitors' refresh rate: {e}")),
            None => Ok(()),
        }
    }

    /// The monitors' sizes and scales, the fastest one's refresh rate and the main one's DPI,
    /// as last recorded.
    type Recorded = (Vec<Monitor>, Option<u32>, u32);

    /// Keeps the record up to date from a loop that runs anyway.
    #[derive(Default)]
    pub struct MonitorWatch {
        last: Option<Recorded>,
    }

    impl MonitorWatch {
        /// Records the monitors if they, the fastest one's refresh rate or the main one's
        /// scale changed since the last call. A failure is returned once, not on every call.
        pub fn check(&mut self) -> Result<(), String> {
            let now = (current(), current_refresh(), main_monitor_dpi());
            if self.last.as_ref() == Some(&now) {
                return Ok(());
            }
            let result = record(&now.0, now.1, now.2);
            self.last = Some(now);
            result
        }
    }
}
