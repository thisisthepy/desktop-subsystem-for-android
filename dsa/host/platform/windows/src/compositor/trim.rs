//! Giving back the memory the window manager is not using while no task window is shown.
//!
//! The window manager holds every task's frames. While windows are shown
//! that memory is in use; once the last one has closed or been minimized, most of it is not,
//! but Windows takes it back only when it runs short, so until then it counts as the window
//! manager's. As soon as no task window is shown ([`IdleTrim`]), and again every five minutes
//! while none is (`IDLE_TRIM_REPEAT`), the window manager asks the GPU driver to free what it keeps for its device beyond live resources,
//! decommits the free memory of every heap in the process (heaps keep freed blocks committed
//! for the next allocation, so a window's worth of freed buffers would otherwise stay charged
//! to the process), and empties its working set: pages it touches again come back on their
//! own, and the rest go to the standby list, free for anything else on the host. Emptying the
//! working set alone frees nothing: the pages stay committed, only paged out. The renderer
//! itself stays: the guest's surfaces live in it, and a window that opens again shows at once.
//!
//! While windows are shown, the GPU driver's spare memory and the default heap's free memory go
//! back too, and the working set stays: when the shown windows are still, every five minutes
//! whether or not they draw, and a few seconds after the last video decoder closed (the
//! decoders, their Direct3D device and Media Foundation free their memory into the heap and the
//! driver, which keep it committed until asked to give it back).
//!
//! Each give-back logs what the process still holds (working set, private commit and GPU
//! memory) and its committed memory by owner (`committed`), so a log shows where the window
//! manager's memory goes. The graphics renderer runs in its own process (`dsa-gfx`): it is asked
//! to give back what it does not use at the same moment, and its own line says what it holds.
//!
//! [`IdleTrim`]: dsa_host_core::memory::IdleTrim

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::memory::{committed_line, held_line, HeldMemory, IdleTrim};
use dsa_host_core::renderer::renderer_line;
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
fn by_owner(held: &HeldMemory) -> String {
    committed_line(&committed::breakdown(
        held.private_kb,
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
    note(logger, &by_owner(&after));
    // The renderer's process gives back what it does not use too, and says what it holds.
    if let Some(renderer) = renderer {
        note(logger, &renderer_line(renderer.give_back().as_ref()));
    }
}

/// Gives back the GPU driver's spare memory and the default heap's free memory while windows
/// may be shown, and logs why (`reason`), what the process held before and holds after, and
/// whose the rest is. The working set stays, so the next frame finds its pages resident.
fn give_back_spare(
    gpu: Option<&Gpu>,
    renderer: Option<&dyn DisplayPresenter>,
    logger: Option<&CompositorLogger>,
    reason: &str,
) {
    let before = held(gpu);
    if let Some(gpu) = gpu {
        gpu.trim();
    }
    committed::compact_heaps();
    let after = held(gpu);
    note(
        logger,
        &format!(
            "{reason}: gave back the GPU driver's spare memory and the heap's free memory; {}",
            held_line(&before, &after)
        ),
    );
    note(logger, &by_owner(&after));
    if let Some(renderer) = renderer {
        note(logger, &renderer_line(renderer.memory().as_ref()));
    }
}

/// Checks `idle` every second until `stop` is set, and gives memory back when it says so.
/// `quiet_for` says how long ago the last frame arrived: while windows are shown but still,
/// only the GPU driver's spare memory goes back, and the working set stays, so the next frame
/// finds its pages resident. `let_go` is set when the window manager let go of something
/// large (its video decoders, once the last video closed); what that freed goes back a few
/// seconds later, whether or not windows are shown.
pub fn run(
    idle: Arc<Mutex<IdleTrim>>,
    gpu: Option<Arc<Gpu>>,
    renderer: Option<Arc<dyn DisplayPresenter>>,
    quiet_for: Box<dyn Fn() -> Duration + Send>,
    let_go: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(POLL);
        let (due, quiet_due, spare_due) = {
            let mut idle = idle.lock().unwrap_or_else(|e| e.into_inner());
            let now = Instant::now();
            if let_go.swap(false, Ordering::SeqCst) {
                idle.let_go(now);
            }
            let spare_due = idle.spare_due(now);
            (idle.due(now), idle.quiet_due(quiet_for()), spare_due)
        };
        if due {
            give_back(gpu.as_deref(), renderer.as_deref(), logger.as_deref());
        } else if quiet_due {
            give_back_spare(
                gpu.as_deref(),
                renderer.as_deref(),
                logger.as_deref(),
                "the shown windows are still",
            );
        } else if spare_due {
            give_back_spare(
                gpu.as_deref(),
                renderer.as_deref(),
                logger.as_deref(),
                "windows are shown, or the last video decoder closed",
            );
        }
    }
}
