//! Giving back the memory the window manager is not using while no task window is shown.
//!
//! The window manager holds the renderer and every task's frames. While windows are shown
//! that memory is in use; once the last one has closed or been minimized, most of it is not,
//! but Windows takes it back only when it runs short, so until then it counts as the window
//! manager's. As soon as no task window is shown ([`IdleTrim`]), the window
//! manager asks the GPU driver to free what it keeps for its device beyond live resources,
//! decommits the free memory of every heap in the process (heaps keep freed blocks committed
//! for the next allocation, so a window's worth of freed buffers would otherwise stay charged
//! to the process), and empties its working set: pages it touches again come back on their
//! own, and the rest go to the standby list, free for anything else on the host. Emptying the
//! working set alone frees nothing: the pages stay committed, only paged out. The renderer
//! itself stays: the guest's surfaces live in it, and a window that opens again shows at once.
//!
//! Each give-back logs what the process still holds (working set, private commit and GPU
//! memory) and its committed memory by owner (`committed`), so a log shows where the window
//! manager's memory goes.
//!
//! [`IdleTrim`]: dsa_host_core::memory::IdleTrim

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::memory::{committed_line, held_line, HeldMemory, IdleTrim};
use windows_sys::Win32::System::ProcessStatus::{
    K32EmptyWorkingSet, K32GetProcessMemoryInfo, PROCESS_MEMORY_COUNTERS,
    PROCESS_MEMORY_COUNTERS_EX,
};
use windows_sys::Win32::System::Threading::GetCurrentProcess;

use super::adapter::DisplayPresenter;
use super::committed;
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

/// The log line that says whose this process's committed memory is.
fn by_owner(held: &HeldMemory, renderer: Option<&dyn DisplayPresenter>) -> String {
    committed_line(&committed::breakdown(
        held.private_kb,
        renderer.and_then(|renderer| renderer.memory()),
        (held.gpu_local_kb, held.gpu_non_local_kb),
    ))
}

/// Trims the GPU driver's memory for `gpu`, decommits the free memory of every heap, empties
/// this process's working set, and logs what the process held before and holds after, and
/// whose the rest is.
pub fn give_back(
    gpu: Option<&Gpu>,
    renderer: Option<&dyn DisplayPresenter>,
    logger: Option<&CompositorLogger>,
) {
    let before = held(gpu);
    if let Some(gpu) = gpu {
        gpu.trim();
    }
    committed::compact_heaps();
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
    note(logger, &by_owner(&after, renderer));
}

/// Checks `idle` every second until `stop` is set, and gives memory back when it says so.
/// `quiet_for` says how long ago the last frame arrived: while windows are shown but still,
/// only the GPU driver's spare memory goes back, and the working set stays, so the next frame
/// finds its pages resident.
pub fn run(
    idle: Arc<Mutex<IdleTrim>>,
    gpu: Option<Arc<Gpu>>,
    renderer: Option<Arc<dyn DisplayPresenter>>,
    quiet_for: Box<dyn Fn() -> Duration + Send>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(POLL);
        let (due, quiet_due) = {
            let mut idle = idle.lock().unwrap_or_else(|e| e.into_inner());
            (idle.due(Instant::now()), idle.quiet_due(quiet_for()))
        };
        if due {
            give_back(gpu.as_deref(), renderer.as_deref(), logger.as_deref());
        } else if quiet_due {
            if let Some(gpu) = gpu.as_deref() {
                let before = held(Some(gpu));
                gpu.trim();
                committed::compact_heaps();
                let after = held(Some(gpu));
                note(
                    logger.as_deref(),
                    &format!(
                        "the shown windows are still: gave back the GPU driver's spare memory; {}",
                        held_line(&before, &after)
                    ),
                );
                note(logger.as_deref(), &by_owner(&after, renderer.as_deref()));
            }
        }
    }
}
