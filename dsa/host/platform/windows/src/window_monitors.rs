//! The desktop's monitors as the core's [`DesktopMonitor`]s, for choosing the monitor an app's
//! window opens on ([`dsa_host_core::compositor::window_monitor`]).
//!
//! A monitor is named by its device interface path (`\\?\DISPLAY#GSM5B09#...#{e6f07b5f-...}`),
//! which names the same monitor on the same port whatever order Windows lists the monitors in
//! and across sign-ins. Two monitors of the same model and size have different paths. A
//! monitor whose path cannot be read is named by its display device (`\\.\DISPLAY2`), which
//! stays while the monitors are not plugged in again elsewhere.

use dsa_host_core::compositor::window_monitor::{opening_monitor, DesktopMonitor};
use windows_sys::Win32::Foundation::{HWND, LPARAM, RECT, TRUE};
use windows_sys::Win32::Graphics::Gdi::{
    EnumDisplayDevicesW, EnumDisplayMonitors, GetMonitorInfoW, MonitorFromRect, MonitorFromWindow,
    DISPLAY_DEVICEW, HDC, HMONITOR, MONITORINFO, MONITORINFOEXW, MONITOR_DEFAULTTONEAREST,
};
use windows_sys::Win32::UI::HiDpi::{
    GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2,
    MDT_EFFECTIVE_DPI,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetWindowPlacement, IsIconic, EDD_GET_DEVICE_INTERFACE_NAME, MONITORINFOF_PRIMARY,
    WINDOWPLACEMENT,
};

/// The text of a NUL-terminated UTF-16 buffer.
fn text_of(buffer: &[u16]) -> String {
    let end = buffer.iter().position(|c| *c == 0).unwrap_or(buffer.len());
    String::from_utf16_lossy(&buffer[..end])
}

/// `monitor` as the core describes it. Called on a thread that is per-monitor aware, so the
/// work area is in physical pixels and the DPI the monitor's own.
unsafe fn describe(monitor: HMONITOR) -> Option<DesktopMonitor> {
    let mut info: MONITORINFOEXW = std::mem::zeroed();
    info.monitorInfo.cbSize = std::mem::size_of::<MONITORINFOEXW>() as u32;
    if GetMonitorInfoW(
        monitor,
        &mut info as *mut MONITORINFOEXW as *mut MONITORINFO,
    ) == 0
    {
        return None;
    }
    let device = text_of(&info.szDevice);
    let mut display: DISPLAY_DEVICEW = std::mem::zeroed();
    display.cb = std::mem::size_of::<DISPLAY_DEVICEW>() as u32;
    let path = (EnumDisplayDevicesW(
        info.szDevice.as_ptr(),
        0,
        &mut display,
        EDD_GET_DEVICE_INTERFACE_NAME,
    ) != 0)
        .then(|| text_of(&display.DeviceID))
        .filter(|path| !path.is_empty());
    let (mut x, mut y) = (0u32, 0u32);
    let dpi = if GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &mut x, &mut y) == 0 && x > 0 {
        x
    } else {
        96
    };
    let work = info.monitorInfo.rcWork;
    Some(DesktopMonitor {
        id: path.unwrap_or(device),
        dpi,
        main: info.monitorInfo.dwFlags & MONITORINFOF_PRIMARY != 0,
        work: (work.left, work.top, work.right, work.bottom),
    })
}

unsafe extern "system" fn add_monitor(
    monitor: HMONITOR,
    _: HDC,
    _: *mut RECT,
    list: LPARAM,
) -> windows_sys::core::BOOL {
    let list = &mut *(list as *mut Vec<DesktopMonitor>);
    if let Some(described) = describe(monitor) {
        list.push(described);
    }
    TRUE
}

/// The monitors of this session, in the order Windows lists them.
pub fn desktop_monitors() -> Vec<DesktopMonitor> {
    let mut monitors: Vec<DesktopMonitor> = Vec::new();
    // SAFETY: the callback only runs during the call, while `monitors` lives. A thread that is
    // not per-monitor aware would see every monitor at 96 DPI.
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        EnumDisplayMonitors(
            std::ptr::null_mut(),
            std::ptr::null(),
            Some(add_monitor),
            &mut monitors as *mut Vec<DesktopMonitor> as LPARAM,
        );
        SetThreadDpiAwarenessContext(previous);
    }
    monitors
}

/// The monitor the window `window` (its handle) is on: the one with most of it, or for a minimized window the
/// one it is restored onto.
pub fn monitor_of_window(window: isize) -> Option<DesktopMonitor> {
    let hwnd = window as HWND;
    // SAFETY: plain calls on a window handle; a stale one only fails. The placement and the
    // rectangle are locals.
    unsafe {
        let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
        let monitor = if IsIconic(hwnd) != 0 {
            let mut placement: WINDOWPLACEMENT = std::mem::zeroed();
            placement.length = std::mem::size_of::<WINDOWPLACEMENT>() as u32;
            if GetWindowPlacement(hwnd, &mut placement) != 0 {
                MonitorFromRect(&placement.rcNormalPosition, MONITOR_DEFAULTTONEAREST)
            } else {
                MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)
            }
        } else {
            MonitorFromWindow(hwnd, MONITOR_DEFAULTTONEAREST)
        };
        let described = (!monitor.is_null()).then(|| describe(monitor)).flatten();
        SetThreadDpiAwarenessContext(previous);
        described
    }
}

/// The monitor an app's next window opens on, from the monitors the desktop has now: the one
/// the app's last window closed on (`last`, by ID) while it is connected, else the main one.
pub fn opening_monitor_now(last: Option<&str>) -> Option<DesktopMonitor> {
    opening_monitor(last, &desktop_monitors()).cloned()
}
