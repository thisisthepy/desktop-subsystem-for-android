//! What a task window says about itself to other DSA processes on the computer, for
//! measurements: its task, and when it presented its first frame and when that frame was
//! shown. And the message `dsa tap` sends a task window before a press it injects, so the
//! window's input trace has the press and watches the pixels around it.
//!
//! Each fact is a window property, which any process of the same user can read. Times are on
//! the host's monotonic clock ([`super::log::host_clock`]), which every process shares, in
//! microseconds plus one, so a property that is not set (0) never reads as a time.

use std::ffi::c_void;
use std::time::Duration;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    FindWindowExW, GetPropW, RegisterWindowMessageW, SendMessageTimeoutW, SetPropW,
    SMTO_ABORTIFHUNG,
};

/// The class of every task window.
pub const TASK_WINDOW_CLASS: &str = "DsaAppWindow";

/// The task a window shows: its ID plus one.
const TASK_PROPERTY: &str = "DsaTaskId";
/// When the window presented its first frame.
pub const FIRST_FRAME_PROPERTY: &str = "DsaFirstFramePresented";
/// When the window's first frame was shown on the screen, at the vertical blank.
pub const FIRST_SHOWN_PROPERTY: &str = "DsaFirstFrameShown";

/// The registered message that tells a task window a press is coming at a point: the task
/// in `wParam`, the point in guest pixels in `lParam` (x in the low 16 bits, y in the next
/// 16). The window answers 1 when it is the task's window.
const TRACE_PRESS_MESSAGE: &str = "DsaTracePress";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

/// The registered message number of [`TRACE_PRESS_MESSAGE`].
pub fn trace_press_message() -> u32 {
    let name = wide(TRACE_PRESS_MESSAGE);
    // SAFETY: the name lives through the call.
    unsafe { RegisterWindowMessageW(name.as_ptr()) }
}

/// Packs a point in guest pixels into the `lParam` of the trace press message.
pub fn pack_point((x, y): (u32, u32)) -> isize {
    (x.min(0xffff) | (y.min(0xffff) << 16)) as isize
}

/// The point [`pack_point`] packed.
pub fn unpack_point(lparam: isize) -> (u32, u32) {
    let value = lparam as usize;
    ((value & 0xffff) as u32, ((value >> 16) & 0xffff) as u32)
}

fn set(hwnd: isize, name: &str, value: isize) {
    let name = wide(name);
    // SAFETY: the name lives through the call; a window that is gone fails it harmlessly.
    unsafe {
        SetPropW(hwnd as *mut c_void, name.as_ptr(), value as *mut c_void);
    }
}

fn get(hwnd: isize, name: &str) -> isize {
    let name = wide(name);
    // SAFETY: the name lives through the call; a window that is gone reads as 0.
    unsafe { GetPropW(hwnd as *mut c_void, name.as_ptr()) as isize }
}

/// Says on the window `hwnd` that it shows task `task_id`.
pub fn mark_task(hwnd: isize, task_id: i32) {
    set(hwnd, TASK_PROPERTY, task_id as isize + 1);
}

/// Says on the window `hwnd` that `name` happened at `at` on the host clock.
pub fn mark_time(hwnd: isize, name: &str, at: Duration) {
    set(hwnd, name, at.as_micros() as isize + 1);
}

/// When `name` happened, as the window `hwnd` says, on the host clock.
pub fn read_time(hwnd: isize, name: &str) -> Option<Duration> {
    let value = get(hwnd, name);
    (value > 0).then(|| Duration::from_micros(value as u64 - 1))
}

/// The windows of every task window host of this desktop, with the task each shows.
pub fn task_windows() -> Vec<(isize, i32)> {
    let class = wide(TASK_WINDOW_CLASS);
    let mut found = Vec::new();
    let mut after: *mut c_void = std::ptr::null_mut();
    loop {
        // SAFETY: the class name lives through the call; `after` is the last window found.
        let hwnd = unsafe {
            FindWindowExW(
                std::ptr::null_mut(),
                after,
                class.as_ptr(),
                std::ptr::null(),
            )
        };
        if hwnd.is_null() {
            break;
        }
        let task = get(hwnd as isize, TASK_PROPERTY);
        if task > 0 {
            found.push((hwnd as isize, (task - 1) as i32));
        }
        after = hwnd;
    }
    found
}

/// The window of task `task_id`, when one is open.
pub fn task_window(task_id: i32) -> Option<isize> {
    task_windows()
        .into_iter()
        .find(|(_, task)| *task == task_id)
        .map(|(hwnd, _)| hwnd)
}

/// Tells the window of task `task_id` that a press is coming at `point` in guest pixels.
/// Returns whether the window took it; a window whose input trace is off does not.
pub fn trace_press(task_id: i32, point: (u32, u32)) -> bool {
    let Some(hwnd) = task_window(task_id) else {
        return false;
    };
    let mut answer: usize = 0;
    // SAFETY: a message to a window of another process, which only DSA's task windows
    // handle; `answer` is a local. The window is not one of any other program's.
    let sent = unsafe {
        SendMessageTimeoutW(
            hwnd as *mut c_void,
            trace_press_message(),
            task_id as usize,
            pack_point(point),
            SMTO_ABORTIFHUNG,
            1000,
            &mut answer,
        )
    };
    sent != 0 && answer == 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fr5_a_press_point_survives_the_message() {
        assert_eq!(unpack_point(pack_point((640, 400))), (640, 400));
        assert_eq!(unpack_point(pack_point((0, 0))), (0, 0));
        assert_eq!(unpack_point(pack_point((70000, 3))), (0xffff, 3));
    }
}
