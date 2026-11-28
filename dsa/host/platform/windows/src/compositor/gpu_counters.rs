//! What Windows' GPU performance counters say each process uses: `\GPU Process Memory(*)\Shared
//! Usage` and `Dedicated Usage`, the numbers Task Manager's "Shared GPU memory" and "Dedicated
//! GPU memory" columns show. The video memory manager counts every allocation a process's GPU
//! devices made there, whatever the graphics API (Direct3D in the window manager, Vulkan in the
//! graphics renderer), including those the driver keeps outside the process's address space,
//! which the private commit does not count.
//!
//! The counters are read through PDH with one query each time, from the window manager's trim
//! thread, a few times an hour: opening the query enumerates every process's counter instances,
//! which takes some tens of milliseconds and nothing that a frame waits for.

use std::collections::HashMap;
use std::ptr::{null, null_mut};

use dsa_host_core::memory::GpuProcessMemory;
use windows_sys::Win32::System::Performance::{
    PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetFormattedCounterArrayW,
    PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_FMT_COUNTERVALUE_ITEM_W,
    PDH_FMT_LARGE, PDH_HCOUNTER, PDH_HQUERY, PDH_MORE_DATA,
};

/// The counters, by their English names, which hold on every display language.
const SHARED_USAGE: &str = "\\GPU Process Memory(*)\\Shared Usage";
const DEDICATED_USAGE: &str = "\\GPU Process Memory(*)\\Dedicated Usage";

/// The process a counter instance belongs to: its name starts `pid_<pid>_`, followed by the
/// adapter (`luid_0x..._0x...`) and the physical GPU (`phys_<n>`). One process has an instance
/// for each GPU it uses.
pub fn pid_of_instance(name: &str) -> Option<u32> {
    name.strip_prefix("pid_")?.split('_').next()?.parse().ok()
}

/// What the counters say each of `pids` uses: every instance of a process summed, in KiB. A
/// process with no instance uses no GPU memory and counts 0. `None` when the counters could not
/// be read at all (no GPU counters on this system, or PDH refused).
pub fn gpu_process_memory(pids: &[u32]) -> Option<HashMap<u32, GpuProcessMemory>> {
    let query = Query::open()?;
    let shared = query.add(SHARED_USAGE)?;
    let dedicated = query.add(DEDICATED_USAGE)?;
    // SAFETY: a query this function opened and has not closed.
    if unsafe { PdhCollectQueryData(query.0) } != 0 {
        return None;
    }
    let mut memory: HashMap<u32, GpuProcessMemory> = pids
        .iter()
        .map(|pid| (*pid, GpuProcessMemory::default()))
        .collect();
    for (name, bytes) in values(shared)? {
        if let Some(entry) = pid_of_instance(&name).and_then(|pid| memory.get_mut(&pid)) {
            entry.shared_kb += bytes / 1024;
        }
    }
    for (name, bytes) in values(dedicated)? {
        if let Some(entry) = pid_of_instance(&name).and_then(|pid| memory.get_mut(&pid)) {
            entry.dedicated_kb += bytes / 1024;
        }
    }
    Some(memory)
}

/// A PDH query, closed when dropped.
struct Query(PDH_HQUERY);

impl Query {
    fn open() -> Option<Query> {
        let mut query: PDH_HQUERY = null_mut();
        // SAFETY: no data source (the live counters), and a handle to fill.
        (unsafe { PdhOpenQueryW(null(), 0, &mut query) } == 0).then_some(Query(query))
    }

    /// Adds the counter `path` (English names, a wildcard instance).
    fn add(&self, path: &str) -> Option<PDH_HCOUNTER> {
        let path: Vec<u16> = path.encode_utf16().chain(Some(0)).collect();
        let mut counter: PDH_HCOUNTER = null_mut();
        // SAFETY: an open query, a NUL-terminated path that outlives the call, and a handle to
        // fill.
        (unsafe { PdhAddEnglishCounterW(self.0, path.as_ptr(), 0, &mut counter) } == 0)
            .then_some(counter)
    }
}

impl Drop for Query {
    fn drop(&mut self) {
        // SAFETY: a query opened by Query::open, closed once; its counters go with it.
        unsafe {
            PdhCloseQuery(self.0);
        }
    }
}

/// Every instance of `counter` with valid data, by name, in bytes (a negative value as 0).
fn values(counter: PDH_HCOUNTER) -> Option<Vec<(String, u64)>> {
    let mut size = 0u32;
    let mut count = 0u32;
    // SAFETY: asks only for the size the array needs: no buffer is written.
    let status = unsafe {
        PdhGetFormattedCounterArrayW(counter, PDH_FMT_LARGE, &mut size, &mut count, null_mut())
    };
    if status != PDH_MORE_DATA {
        return (status == 0).then(Vec::new);
    }
    // The size is in bytes: the items, then the instance names they point at.
    let item = std::mem::size_of::<PDH_FMT_COUNTERVALUE_ITEM_W>();
    let mut buffer = vec![PDH_FMT_COUNTERVALUE_ITEM_W::default(); (size as usize).div_ceil(item)];
    // SAFETY: a buffer of at least `size` bytes, aligned for the items it receives.
    let status = unsafe {
        PdhGetFormattedCounterArrayW(
            counter,
            PDH_FMT_LARGE,
            &mut size,
            &mut count,
            buffer.as_mut_ptr(),
        )
    };
    if status != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(count as usize);
    for entry in buffer.iter().take(count as usize) {
        let status = entry.FmtValue.CStatus;
        if status != PDH_CSTATUS_VALID_DATA && status != PDH_CSTATUS_NEW_DATA {
            continue;
        }
        // SAFETY: PDH_FMT_LARGE fills the 64-bit member.
        let bytes = unsafe { entry.FmtValue.Anonymous.largeValue };
        // SAFETY: PDH points each name at a NUL-terminated string inside `buffer`, which lives
        // until the names are copied here.
        let name = unsafe { wide_text(entry.szName) };
        out.push((name, u64::try_from(bytes).unwrap_or(0)));
    }
    Some(out)
}

/// The text of a NUL-terminated UTF-16 string; empty for a null pointer.
///
/// # Safety
/// `text` is null or points at a NUL-terminated UTF-16 string.
unsafe fn wide_text(text: *const u16) -> String {
    if text.is_null() {
        return String::new();
    }
    let mut len = 0;
    // SAFETY: the caller's promise: the string ends with a NUL.
    while unsafe { *text.add(len) } != 0 {
        len += 1;
    }
    // SAFETY: `len` units were just read.
    String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) })
}

#[cfg(test)]
mod tests {
    use super::pid_of_instance;

    #[test]
    fn nfr1_a_gpu_counter_instance_names_its_process() {
        assert_eq!(
            pid_of_instance("pid_76072_luid_0x00000000_0x0000D1C6_phys_0"),
            Some(76072)
        );
        assert_eq!(
            pid_of_instance("pid_4_luid_0x00000000_0x00010A2E_phys_0"),
            Some(4)
        );
        assert_eq!(pid_of_instance("_Total"), None);
        assert_eq!(pid_of_instance("pid__luid"), None);
    }
}
