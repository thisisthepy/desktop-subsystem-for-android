//! Giving back the memory the window manager is not using while no task window is shown.
//!
//! The window manager holds the renderer and every task's frames. While windows are shown
//! that memory is in use; once the last one has closed or been minimized, most of it is not,
//! but Windows takes it back only when it runs short, so until then it counts as the window
//! manager's. When no task window has been shown for a while ([`IdleTrim`]), the window
//! manager asks the GPU driver to free what it keeps for its device beyond live resources, and
//! empties its working set: pages it touches again come back on their own, and the rest go to
//! the standby list, free for anything else on the host. The renderer itself stays: the
//! guest's surfaces live in it, and a window that opens again shows at once.
//!
//! Each give-back logs what the process still holds (working set, private commit and GPU
//! memory), so a log shows where the window manager's memory goes.
//!
//! [`IdleTrim`]: dsa_host_core::memory::IdleTrim

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::memory::{held_line, HeldMemory, IdleTrim};
use windows_sys::Win32::System::ProcessStatus::{
    K32EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::log::{note, CompositorLogger};
use super::present::Gpu;

/// How often the idle check runs, which is also how long a stop waits for it at most.
const POLL: Duration = Duration::from_secs(1);

/// This process's working set and private commit, in KiB.
fn process_kb() -> Option<(u64, u64)> {
    // SAFETY: plain counters, zero until filled.
    let mut counters: PROCESS_MEMORY_COUNTERS_EX = unsafe { std::mem::zeroed() };
    let size = std::mem::size_of::<PROCESS_MEMORY_COUNTERS_EX>() as u32;
    counters.cb = size;
    // SAFETY: our own process's pseudo handle, and counters of the size given; the extended
    // counters start with the plain ones.
    let ok = unsafe {
        K32GetProcessMemoryInfo(
            GetCurrentProcess(),
            &mut counters as *mut PROCESS_MEMORY_COUNTERS_EX as *mut PROCESS_MEMORY_COUNTERS,
            size,
        )
    };
    (ok != 0).then_some((
        counters.WorkingSetSize as u64 / 1024,
        counters.PrivateUsage as u64 / 1024,
    ))
}

fn held(gpu: Option<&Gpu>) -> HeldMemory {
    let (working_set_kb, private_kb) = process_kb().unzip();
    let (gpu_local_kb, gpu_non_local_kb) = gpu.and_then(Gpu::video_memory_kb).unzip();
    HeldMemory {
        working_set_kb,
        private_kb,
        gpu_local_kb,
        gpu_non_local_kb,
    }
}

/// Empties this process's working set: for a window host whose window was just minimized,
/// which shows nothing until it is restored. The pages stay on the standby list, so a restore
/// faults them back without reading the disk unless Windows has reused them.
pub fn empty_working_set() {
    // SAFETY: our own process's pseudo handle.
    unsafe {
        K32EmptyWorkingSet(GetCurrentProcess());
    }
}

/// Trims the GPU driver's memory for `gpu`, empties this process's working set, and logs what
/// the process held before and holds after.
pub fn give_back(gpu: Option<&Gpu>, logger: Option<&CompositorLogger>) {
    let before = held(gpu);
    if let Some(gpu) = gpu {
        gpu.trim();
    }
    // SAFETY: our own process's pseudo handle.
    let emptied = unsafe { K32EmptyWorkingSet(GetCurrentProcess()) } != 0;
    let after = held(gpu);
    let message = if emptied {
        format!(
            "no task window is shown: gave back memory not in use; {}",
            held_line(&before, &after)
        )
    } else {
        format!(
            "no task window is shown, but the working set could not be emptied: {}; {}",
            std::io::Error::last_os_error(),
            held_line(&before, &after)
        )
    };
    note(logger, &message);
}

/// Checks `idle` every second until `stop` is set, and gives memory back when it says so.
pub fn run(
    idle: Arc<Mutex<IdleTrim>>,
    gpu: Option<Arc<Gpu>>,
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
            give_back(gpu.as_deref(), logger.as_deref());
        }
    }
}
