//! Giving back the memory the window manager is not using while no task window is open.
//!
//! The window manager holds the renderer and every task's frames. While windows are open
//! that memory is in use; once the last one has closed, most of it is not, but Windows takes
//! it back only when it runs short, so until then it counts as the window manager's. When no
//! task window has been open for a while ([`IdleTrim`]), the window manager empties its
//! working set: pages it touches again come back on their own, and the rest go to the
//! standby list, free for anything else on the host.
//!
//! [`IdleTrim`]: dsa_host_core::memory::IdleTrim

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::memory::IdleTrim;
use windows_sys::Win32::System::ProcessStatus::{
    K32EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::log::{note, CompositorLogger};

/// How often the idle check runs, which is also how long a stop waits for it at most.
const POLL: Duration = Duration::from_secs(1);

/// This process's working set, in KiB.
fn working_set_kb() -> Option<u64> {
    // SAFETY: plain counters, zero until filled.
    let mut counters: PROCESS_MEMORY_COUNTERS = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS>() as u32;
    counters.cb = size;
    // SAFETY: our own process's pseudo handle, and counters of the size given.
    let ok = unsafe { K32GetProcessMemoryInfo(GetCurrentProcess(), &mut counters, size) };
    (ok != 0).then_some(counters.WorkingSetSize as u64 / 1024)
}

/// Empties this process's working set and logs how much it held before and after.
pub fn give_back(logger: Option<&CompositorLogger>) {
    let before = working_set_kb();
    // SAFETY: our own process's pseudo handle.
    let emptied = unsafe { K32EmptyWorkingSet(GetCurrentProcess()) } != 0;
    let after = working_set_kb();
    let mib = |kb: Option<u64>| kb.map_or("?".to_string(), |kb| (kb / 1024).to_string());
    let message = if emptied {
        format!(
            "no task window is open: gave back memory not in use (working set {} MiB, now {} MiB)",
            mib(before),
            mib(after)
        )
    } else {
        format!(
            "no task window is open, but the working set could not be emptied: {}",
            std::io::Error::last_os_error()
        )
    };
    note(logger, &message);
}

/// Checks `idle` every second until `stop` is set, and gives memory back when it says so.
pub fn run(
    idle: Arc<Mutex<IdleTrim>>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(POLL);
        let due = idle
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .due(Instant::now());
        if due {
            give_back(logger.as_deref());
        }
    }
}
