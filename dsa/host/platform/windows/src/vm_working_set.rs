//! Emptying the working set of a VM's memory process (`vmmem`), for the core's idle VM trim
//! ([`dsa_host_core::idle_vm_trim`]).
//!
//! Hyper-V keeps each VM's memory in a `vmmem` process (`vmmemDSA` for DSA's VMs).
//! `EmptyWorkingSet` on it takes the guest's pages out of RAM: the private ones go to the modified
//! list, and from there to the page file, and the clean ones (the disk files the VM maps directly)
//! to the standby list. A page the guest touches again comes back by itself. The process is
//! protected: only an administrator or SYSTEM opens it with `PROCESS_QUERY_INFORMATION |
//! PROCESS_SET_QUOTA`, which the call needs. The service runs as SYSTEM.
//!
//! What the VM holds in RAM is its private working set, the figure Task Manager's Memory column
//! shows. What the host writes to its page file is the raw value of the system's
//! `\Memory\Pages Output/sec` counter: every page written out to free memory since Windows
//! started. Windows keeps no count of page file writes for one process, because the modified page
//! writer writes them from the System process, so the VM process's own I/O counters never see
//! them. The system's count also takes in the changed pages of mapped files that are written
//! back, which an idle host writes little of, so its growth after a trim is what the trim cost.

use std::collections::HashMap;
use std::sync::Mutex;

use dsa_host_core::idle_vm_trim::{EmptyRefused, VmMemoryLook, VmWorkingSet};

/// The VM processes of the running VMs, found by their VM IDs.
#[derive(Default)]
pub struct WindowsVmWorkingSet {
    /// The `vmmem` process of each VM seen, by VM ID. A boot has a new ID, so only the newest
    /// is kept.
    pids: Mutex<HashMap<String, u32>>,
}

impl WindowsVmWorkingSet {
    pub fn new() -> WindowsVmWorkingSet {
        WindowsVmWorkingSet::default()
    }

    /// The `vmmem` process of the VM `vm_id`: the one found before, or, when there is none or
    /// `fresh` is set, the one the system's process list has now.
    fn pid(&self, vm_id: &str, fresh: bool) -> Option<u32> {
        let mut pids = self.pids.lock().unwrap_or_else(|e| e.into_inner());
        if !fresh {
            if let Some(pid) = pids.get(vm_id) {
                return Some(*pid);
            }
        }
        let pid = crate::host_memory::vm_processes(vm_id)
            .ok()?
            .into_iter()
            .next()?;
        pids.clear();
        pids.insert(vm_id.to_string(), pid);
        Some(pid)
    }

    /// `read` of the VM's process, looked up again once if the process found before no longer
    /// answers.
    fn with_process<T>(&self, vm_id: &str, read: impl Fn(u32) -> Option<T>) -> Option<T> {
        if let Some(value) = self.pid(vm_id, false).and_then(&read) {
            return Some(value);
        }
        self.pid(vm_id, true).and_then(read)
    }
}

impl VmWorkingSet for WindowsVmWorkingSet {
    fn look(&self, vm_id: &str) -> VmMemoryLook {
        VmMemoryLook {
            resident_kb: self.with_process(vm_id, crate::host_memory::private_working_set_kb),
            paged_out_kb: system::pages_output_kb(),
        }
    }

    fn empty(&self, vm_id: &str) -> Result<(), EmptyRefused> {
        let Some(pid) = self.pid(vm_id, false) else {
            return Err(EmptyRefused::Failed(
                "the VM's memory process (vmmem) is not among the host's processes".into(),
            ));
        };
        match system::empty_working_set(pid) {
            Ok(()) => Ok(()),
            Err(first) => {
                // The process found before may have gone with a boot that ended.
                let Some(again) = self.pid(vm_id, true).filter(|again| *again != pid) else {
                    return Err(EmptyRefused::Failed(format!("vmmem pid {pid}: {first}")));
                };
                system::empty_working_set(again)
                    .map_err(|error| EmptyRefused::Failed(format!("vmmem pid {again}: {error}")))
            }
        }
    }
}

mod system {
    use std::io;
    use std::ptr::{null, null_mut};

    use windows_sys::Win32::Foundation::HANDLE;
    use windows_sys::Win32::System::Performance::{
        PdhAddEnglishCounterW, PdhCloseQuery, PdhCollectQueryData, PdhGetRawCounterValue,
        PdhOpenQueryW, PDH_CSTATUS_NEW_DATA, PDH_CSTATUS_VALID_DATA, PDH_HCOUNTER, PDH_HQUERY,
        PDH_RAW_COUNTER,
    };
    use windows_sys::Win32::System::ProcessStatus::K32EmptyWorkingSet;
    use windows_sys::Win32::System::SystemInformation::{GetSystemInfo, SYSTEM_INFO};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_INFORMATION, PROCESS_SET_QUOTA,
    };

    use crate::pipe::OwnedHandle;

    /// The system's count of pages written out to free memory, by its English name, which holds
    /// on every display language.
    const PAGES_OUTPUT: &str = "\\Memory\\Pages Output/sec";

    /// Empties the working set of the process `pid`.
    pub fn empty_working_set(pid: u32) -> io::Result<()> {
        // SAFETY: opening a process by ID with the rights EmptyWorkingSet needs; closed by
        // OwnedHandle.
        let handle: HANDLE =
            unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_SET_QUOTA, 0, pid) };
        if handle.is_null() {
            return Err(io::Error::last_os_error());
        }
        let process = OwnedHandle(handle);
        // SAFETY: a process handle opened above with the rights the call needs.
        if unsafe { K32EmptyWorkingSet(process.0) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// A PDH query, closed when dropped.
    struct Query(PDH_HQUERY);

    impl Drop for Query {
        fn drop(&mut self) {
            // SAFETY: a query PdhOpenQueryW opened, closed once; its counter goes with it.
            unsafe {
                PdhCloseQuery(self.0);
            }
        }
    }

    /// Every page Windows has written out to free memory since it started, in KiB; `None` when
    /// the counter cannot be read.
    pub fn pages_output_kb() -> Option<u64> {
        let mut query: PDH_HQUERY = null_mut();
        // SAFETY: no data source (the live counters), and a handle to fill.
        if unsafe { PdhOpenQueryW(null(), 0, &mut query) } != 0 {
            return None;
        }
        let query = Query(query);
        let path: Vec<u16> = PAGES_OUTPUT.encode_utf16().chain(Some(0)).collect();
        let mut counter: PDH_HCOUNTER = null_mut();
        // SAFETY: an open query, a NUL-terminated path that outlives the call, and a handle to
        // fill.
        if unsafe { PdhAddEnglishCounterW(query.0, path.as_ptr(), 0, &mut counter) } != 0 {
            return None;
        }
        // SAFETY: the query opened above.
        if unsafe { PdhCollectQueryData(query.0) } != 0 {
            return None;
        }
        let mut raw = PDH_RAW_COUNTER::default();
        // SAFETY: a counter of the query, collected above; the type is not asked for.
        if unsafe { PdhGetRawCounterValue(counter, null_mut(), &mut raw) } != 0 {
            return None;
        }
        if raw.CStatus != PDH_CSTATUS_VALID_DATA && raw.CStatus != PDH_CSTATUS_NEW_DATA {
            return None;
        }
        // The raw value of a rate counter is the count it is the rate of.
        let pages = u64::try_from(raw.FirstValue).ok()?;
        Some(pages * page_kb())
    }

    /// The size of a page, in KiB.
    fn page_kb() -> u64 {
        // SAFETY: plain data, filled by the call.
        let mut info: SYSTEM_INFO = unsafe { std::mem::zeroed() };
        // SAFETY: a structure of the size the call fills.
        unsafe { GetSystemInfo(&mut info) };
        (u64::from(info.dwPageSize) / 1024).max(1)
    }
}
