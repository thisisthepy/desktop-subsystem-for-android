//! Where the window manager's committed memory goes, by owner, and giving back the heap
//! memory nothing uses.
//!
//! Task Manager shows a process's private commit as one number. To say whose it is, the window
//! manager walks its own address space: every committed private page, split into the default
//! heap (the window manager's own allocations go there), pages mapped write-combined or uncached
//! (only GPU drivers map those), and the rest, which drivers and the renderer allocate straight
//! from the system or keep in heaps of their own; those heaps are only counted. Beside that go
//! the bytes the window manager's own allocator has handed out ([`CountingAllocator`]), the
//! renderer's count of its GPU allocations, and the GPU memory the process uses. The breakdown
//! is logged with every give-back (see `trim`), so a log from a real machine says where the
//! memory is.

use std::alloc::{GlobalAlloc, Layout, System};
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};

use dsa_host_core::memory::{CommittedMemory, RendererMemory};
use windows_sys::Win32::Foundation::HANDLE;
use windows_sys::Win32::System::Memory::{
    GetProcessHeap, GetProcessHeaps, HeapCompact, HeapSummary, VirtualQuery, HEAP_SUMMARY,
    MEMORY_BASIC_INFORMATION, MEM_COMMIT, MEM_PRIVATE, PAGE_NOCACHE, PAGE_WRITECOMBINE,
};

/// How many of the largest private allocations a breakdown names.
const LARGEST: usize = 6;

static LIVE_BYTES: AtomicU64 = AtomicU64::new(0);
static COUNTING: AtomicU64 = AtomicU64::new(0);

/// The system allocator, counting the bytes it has handed out and not had back. The window
/// manager's binary installs it as its global allocator, so a breakdown can tell the window
/// manager's own memory from what drivers and the renderer hold in the same heap.
pub struct CountingAllocator;

// SAFETY: every call goes to the system allocator unchanged; only a counter is added.
unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        COUNTING.store(1, Ordering::Relaxed);
        let ptr = System.alloc(layout);
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        COUNTING.store(1, Ordering::Relaxed);
        let ptr = System.alloc_zeroed(layout);
        if !ptr.is_null() {
            LIVE_BYTES.fetch_add(layout.size() as u64, Ordering::Relaxed);
        }
        ptr
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        System.dealloc(ptr, layout);
        LIVE_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let moved = System.realloc(ptr, layout, new_size);
        if !moved.is_null() {
            LIVE_BYTES.fetch_sub(layout.size() as u64, Ordering::Relaxed);
            LIVE_BYTES.fetch_add(new_size as u64, Ordering::Relaxed);
        }
        moved
    }
}

/// The bytes the window manager's own allocator holds, in KiB, when it counts them.
fn own_live_kb() -> Option<u64> {
    (COUNTING.load(Ordering::Relaxed) != 0).then(|| LIVE_BYTES.load(Ordering::Relaxed) / 1024)
}

/// Every heap of this process; the default heap first.
fn heaps() -> Vec<HANDLE> {
    // SAFETY: a count query with no buffer, then a buffer of the size it gave; heaps made in
    // between are left out, as the second call says.
    unsafe {
        let count = GetProcessHeaps(0, std::ptr::null_mut());
        let mut all: Vec<HANDLE> = vec![std::ptr::null_mut(); count as usize + 8];
        let got = GetProcessHeaps(all.len() as u32, all.as_mut_ptr()) as usize;
        all.truncate(got.min(all.len()));
        let default = GetProcessHeap();
        all.sort_by_key(|heap| *heap != default);
        all
    }
}

/// Gives back the free memory of the process's default heap, where the window manager's own
/// allocations live: a heap keeps freed blocks committed for the next allocation, and
/// compacting it decommits what is free. The other heaps are left alone: a library may have
/// made its heap without locking (HEAP_NO_SERIALIZE), and compacting or reading such a heap
/// while its owner allocates from it corrupts it.
pub fn compact_heaps() {
    // SAFETY: the default heap always serializes its callers, so compacting takes its lock
    // like any allocation does.
    unsafe {
        HeapCompact(GetProcessHeap(), 0);
    }
}

/// How much one heap has committed, in bytes.
fn heap_committed(heap: HANDLE) -> u64 {
    // SAFETY: a summary of the documented size for a heap of this process.
    unsafe {
        let mut summary: HEAP_SUMMARY = std::mem::zeroed();
        summary.cb = std::mem::size_of::<HEAP_SUMMARY>() as u32;
        if HeapSummary(heap, 0, &mut summary) == 0 {
            return 0;
        }
        summary.cbCommitted as u64
    }
}

/// The committed private pages of this process: in all, mapped for the GPU, and grouped by
/// the allocation they belong to, in bytes.
fn walk() -> (u64, u64, Vec<u64>) {
    let mut total = 0u64;
    let mut gpu = 0u64;
    let mut by_allocation: HashMap<usize, u64> = HashMap::new();
    let mut address = 0usize;
    loop {
        // SAFETY: a query of this process's own address space into a local of the size given.
        let mut info: MEMORY_BASIC_INFORMATION = unsafe { std::mem::zeroed() };
        let size = std::mem::size_of::<MEMORY_BASIC_INFORMATION>();
        let got = unsafe { VirtualQuery(address as *const _, &mut info, size) };
        if got == 0 || info.RegionSize == 0 {
            break;
        }
        if info.State == MEM_COMMIT && info.Type == MEM_PRIVATE {
            let bytes = info.RegionSize as u64;
            total += bytes;
            if info.Protect & (PAGE_WRITECOMBINE | PAGE_NOCACHE) != 0 {
                gpu += bytes;
            }
            *by_allocation
                .entry(info.AllocationBase as usize)
                .or_default() += bytes;
        }
        match (info.BaseAddress as usize).checked_add(info.RegionSize) {
            Some(next) if next > address => address = next,
            _ => break,
        }
    }
    let mut largest: Vec<u64> = by_allocation.into_values().collect();
    largest.sort_unstable_by(|a, b| b.cmp(a));
    largest.truncate(LARGEST);
    (total, gpu, largest)
}

/// This process's committed memory by owner. `private_kb` and the GPU's figures come from
/// the caller, which reads them anyway.
pub fn breakdown(
    private_kb: Option<u64>,
    renderer: Option<RendererMemory>,
    gpu: (Option<u64>, Option<u64>),
) -> CommittedMemory {
    let all = heaps();
    let process_heap = all.first().map_or(0, |heap| heap_committed(*heap));
    // Other heaps may not lock (see compact_heaps), so they are counted, not read; their
    // pages are among what was allocated directly.
    let other_heaps = 0u64;
    let (walked, gpu_mapped, largest) = walk();
    CommittedMemory {
        private_kb,
        walked_private_kb: walked / 1024,
        process_heap_kb: process_heap / 1024,
        other_heaps_kb: other_heaps / 1024,
        other_heaps: all.len().saturating_sub(1) as u32,
        gpu_mapped_kb: gpu_mapped / 1024,
        direct_kb: walked.saturating_sub(process_heap + other_heaps + gpu_mapped) / 1024,
        largest_private_kb: largest.into_iter().map(|bytes| bytes / 1024).collect(),
        own_live_kb: own_live_kb(),
        renderer,
        gpu_local_kb: gpu.0,
        gpu_non_local_kb: gpu.1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nfr2_a_breakdown_finds_a_large_private_allocation_and_the_heaps() {
        // A 32 MiB block, committed and touched, shows up among the largest allocations.
        let block = vec![1u8; 32 << 20];
        let found = breakdown(None, None, (None, None));
        assert!(found.walked_private_kb >= 32 * 1024, "{found:?}");
        assert!(
            found.largest_private_kb.iter().any(|kb| *kb >= 32 * 1024),
            "{found:?}"
        );
        assert!(found.process_heap_kb > 0, "{found:?}");
        assert!(found.walked_private_kb >= found.direct_kb, "{found:?}");
        assert!(block.iter().all(|byte| *byte == 1));
        drop(block);
        compact_heaps();
    }

    #[test]
    fn nfr2_a_give_back_while_other_threads_allocate_corrupts_no_heap() {
        use std::sync::atomic::AtomicBool;
        use std::sync::Arc;
        let stop = Arc::new(AtomicBool::new(false));
        let workers: Vec<_> = (0..8)
            .map(|seed| {
                let stop = Arc::clone(&stop);
                std::thread::spawn(move || {
                    let mut kept: Vec<Vec<u8>> = Vec::new();
                    let mut n = seed as usize;
                    while !stop.load(Ordering::Relaxed) {
                        n = n.wrapping_mul(1103515245).wrapping_add(12345);
                        let mut block = vec![seed as u8; 16 + n % 200_000];
                        block.resize(block.len() + n % 4096, 1);
                        kept.push(block);
                        if kept.len() > 64 {
                            kept.drain(..32);
                        }
                    }
                    kept.len()
                })
            })
            .collect();
        for _ in 0..200 {
            compact_heaps();
            let found = breakdown(None, None, (None, None));
            assert!(found.walked_private_kb > 0);
        }
        stop.store(true, Ordering::Relaxed);
        for worker in workers {
            worker
                .join()
                .expect("a worker thread survives the give-backs");
        }
    }

    #[test]
    fn nfr2_the_counting_allocator_keeps_the_global_alloc_contract() {
        // Alignment, zeroing and a growing and shrinking realloc go to the system allocator
        // unchanged, and the count returns to where it was.
        let a = CountingAllocator;
        let before = LIVE_BYTES.load(Ordering::Relaxed);
        for align in [1usize, 8, 64, 4096] {
            let layout = Layout::from_size_align(100, align).unwrap();
            // SAFETY: a non-zero layout, freed with the layout it was made with.
            unsafe {
                let p = a.alloc_zeroed(layout);
                assert!(!p.is_null());
                assert_eq!(p as usize % align, 0);
                assert!(std::slice::from_raw_parts(p, 100).iter().all(|b| *b == 0));
                let grown = a.realloc(p, layout, 10_000);
                assert!(!grown.is_null());
                assert_eq!(grown as usize % align, 0);
                let grown_layout = Layout::from_size_align(10_000, align).unwrap();
                let shrunk = a.realloc(grown, grown_layout, 10);
                assert!(!shrunk.is_null());
                a.dealloc(shrunk, Layout::from_size_align(10, align).unwrap());
            }
        }
        assert_eq!(LIVE_BYTES.load(Ordering::Relaxed), before);
    }
}
