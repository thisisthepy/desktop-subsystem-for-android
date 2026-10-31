//! How Windows schedules the processes and threads a touch passes through.
//!
//! Windows treats a process with no focused window as background work. The window manager has
//! no window at all, the service is a service, and an app's window host stops being in focus
//! as soon as the user looks at another window, though its frames still show. On a machine
//! with efficiency cores or on battery, such a process can run on the slow cores with its
//! timers coalesced, while the user waits for a frame or a touch that passes through it.

use windows_sys::Win32::System::Threading::{
    GetCurrentProcess, GetCurrentThread, ProcessPowerThrottling, SetPriorityClass,
    SetProcessInformation, SetThreadPriority, ABOVE_NORMAL_PRIORITY_CLASS,
    PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
    PROCESS_POWER_THROTTLING_STATE, THREAD_PRIORITY_HIGHEST,
};

/// Keeps this process off the efficiency scheduling Windows gives background work, and ahead
/// of normal programs. What it says went wrong costs speed only, so callers may log and go on.
pub fn raise_this_process() -> Result<(), String> {
    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        // Controlled and not set: never throttled.
        StateMask: 0,
    };
    // SAFETY: the state outlives the call and has the size passed; the pseudo handle needs no
    // closing.
    let throttling = unsafe {
        SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&state as *const PROCESS_POWER_THROTTLING_STATE).cast(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        )
    };
    if throttling == 0 {
        return Err(format!(
            "could not keep this process off efficiency scheduling: {}",
            std::io::Error::last_os_error()
        ));
    }
    // SAFETY: the pseudo handle of this process.
    if unsafe { SetPriorityClass(GetCurrentProcess(), ABOVE_NORMAL_PRIORITY_CLASS) } == 0 {
        return Err(format!(
            "could not raise this process's priority: {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Raises the calling thread, for the short work a touch or a frame waits on directly: such a
/// thread spends nearly all its time blocked.
pub fn raise_this_thread() {
    // SAFETY: the pseudo handle of this thread. A failure costs speed only.
    unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_HIGHEST) };
}
