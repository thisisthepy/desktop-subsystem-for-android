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
//! driver, which keep it committed until asked to give it back). With each give-back the pages of
//! the video pictures shared with the guest that no picture holds now go back as well: they are
//! mapped memory, in the working set but not in the private commit, and a player kept open
//! otherwise held every page its slots ever reached.
//!
//! Each give-back logs what the process still holds (working set, private commit and GPU
//! memory) and its committed memory by owner (`committed`), so a log shows where the window
//! manager's memory goes. The graphics renderer runs in its own process (`dsa-gfx`): it is asked
//! to give back what it does not use at the same moment, and its own line says what it holds.
//!
//! Once Android has stopped (not dozing: [`SharedPresence`] says when), the window manager
//! holds nothing for it: the renderer process has ended, and the Direct3D device the task
//! windows' textures were made on goes too ([`Gpu::park`]), with the driver's memory for it.
//! The heap's free memory and the working set go back after it. The next app that opens
//! starts Android, the renderer starts with it, and the device is made again when the first
//! window or frame needs it.
//!
//! [`IdleTrim`]: dsa_host_core::memory::IdleTrim
//! [`SharedPresence`]: dsa_host_core::android_presence::SharedPresence

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use dsa_host_core::android_presence::SharedPresence;
use dsa_host_core::memory::{committed_line, held_line, HeldMemory, IdleTrim};
use dsa_host_core::renderer::renderer_line;
use dsa_host_core::video_slots::{give_back_unused_pictures, PicturesGivenBack};
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

/// Gives back what a window host holds for a window that was just minimized: the free memory
/// of every heap, and then the working set. A window host has no GPU device of its own; the
/// window manager lets the window's swapchain go and trims its own driver memory. Emptying the
/// working set alone only pages memory out: the heap decommit is what takes private commit
/// down.
pub fn give_back_minimized() {
    committed::compact_heaps();
    empty_working_set();
}

/// What the give-back said of the video pictures shared with the guest: mapped memory, which
/// the working set counts and the private commit does not. Empty while no video has played.
fn pictures_line(pictures: PicturesGivenBack) -> String {
    if pictures.touched == 0 && pictures.given_back == 0 {
        return String::new();
    }
    format!(
        ", and {} of the {} of shared picture memory the video slots had reached that no \
         picture holds now",
        mib(pictures.given_back),
        mib(pictures.touched)
    )
}

fn mib(bytes: usize) -> String {
    format!("{} MiB", bytes / (1024 * 1024))
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
    give_back_because(gpu, renderer, logger, "no task window is shown");
}

/// [`give_back`], saying `why` in the log.
fn give_back_because(
    gpu: Option<&Gpu>,
    renderer: Option<&dyn DisplayPresenter>,
    logger: Option<&CompositorLogger>,
    why: &str,
) {
    let before = held(gpu);
    if let Some(gpu) = gpu {
        gpu.trim();
    }
    committed::compact_heaps();
    let pictures = give_back_unused_pictures();
    // SAFETY: our own process's pseudo handle.
    let emptied = unsafe { K32EmptyWorkingSet(GetCurrentProcess()) } != 0;
    let after = held(gpu);
    let message = if emptied {
        format!(
            "{why}: gave back memory not in use{}; {}",
            pictures_line(pictures),
            held_line(&before, &after)
        )
    } else {
        format!(
            "{why}, but the working set could not be emptied: {}; {}",
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
    let pictures = give_back_unused_pictures();
    let after = held(gpu);
    note(
        logger,
        &format!(
            "{reason}: gave back the GPU driver's spare memory and the heap's free memory{}; {}",
            pictures_line(pictures),
            held_line(&before, &after)
        ),
    );
    note(logger, &by_owner(&after));
    if let Some(renderer) = renderer {
        note(logger, &renderer_line(renderer.memory().as_ref()));
    }
}

/// Lets go of what the window manager holds only for a running Android, once `presence` says
/// it has stopped: the Direct3D device (when no window holds it), then the heap's free memory
/// and the working set. Done once for each stop, and again whenever a device made meanwhile
/// could be let go of.
fn release_while_stopped(
    presence: &SharedPresence,
    gpu: Option<&Gpu>,
    renderer: Option<&dyn DisplayPresenter>,
    logger: Option<&CompositorLogger>,
) {
    if !presence.settled_stopped() {
        return;
    }
    let parked = gpu.is_some_and(Gpu::park);
    if presence.release_due() || parked {
        let why = if parked {
            "Android is stopped; let go of the task windows' Direct3D device"
        } else {
            "Android is stopped"
        };
        give_back_because(gpu, renderer, logger, why);
    }
}

/// What the trim gives back memory of: the task windows' devices, the renderer process, and
/// whether Android runs (`None` where the window manager does not follow it).
pub struct Holdings {
    pub gpu: Option<Arc<Gpu>>,
    pub renderer: Option<Arc<dyn DisplayPresenter>>,
    pub presence: Option<Arc<SharedPresence>>,
}

/// Checks `idle` every second until `stop` is set, and gives memory back when it says so.
/// `quiet_for` says how long ago the last frame arrived: while windows are shown but still,
/// only the GPU driver's spare memory goes back, and the working set stays, so the next frame
/// finds its pages resident. `let_go` is set when the window manager let go of something
/// large (its video decoders, once the last video closed); what that freed goes back a few
/// seconds later, whether or not windows are shown.
pub fn run(
    idle: Arc<Mutex<IdleTrim>>,
    held: Holdings,
    quiet_for: Box<dyn Fn() -> Duration + Send>,
    let_go: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    logger: Option<Arc<CompositorLogger>>,
) {
    let Holdings {
        gpu,
        renderer,
        presence,
    } = held;
    while !stop.load(Ordering::Relaxed) {
        thread::sleep(POLL);
        if let Some(presence) = &presence {
            release_while_stopped(
                presence,
                gpu.as_deref(),
                renderer.as_deref(),
                logger.as_deref(),
            );
        }
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
