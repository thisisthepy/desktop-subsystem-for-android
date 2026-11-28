//! The idle VM trim on macOS ([`dsa_host_core::idle_vm_trim`]): it cannot be done here, and the
//! log says why, once.
//!
//! macOS has no call that takes another process's memory out of RAM. The VM runs in Apple's
//! Virtualization service process (`com.apple.Virtualization.VirtualMachine`), a platform binary
//! whose task port the system gives no other program, so its address space cannot be advised from
//! outside; `madvise` acts only on the caller's own memory. What leaves RAM is the kernel's
//! choice alone (its memory compressor and swap, under pressure). The guest's free memory still
//! goes back through the balloon's free page reporting.

use dsa_host_core::idle_vm_trim::{EmptyRefused, VmMemoryLook, VmWorkingSet};

use crate::host_memory::{process_memory_kb, VmProcess};

/// Says that the VM's memory cannot be paged out from outside, and what the VM holds.
pub struct MacVmWorkingSet {
    pub vm: VmProcess,
}

impl VmWorkingSet for MacVmWorkingSet {
    fn look(&self, _vm_id: &str) -> VmMemoryLook {
        VmMemoryLook {
            resident_kb: self
                .vm
                .get()
                .and_then(process_memory_kb)
                .map(|(resident, _)| resident),
            paged_out_kb: None,
        }
    }

    fn empty(&self, _vm_id: &str) -> Result<(), EmptyRefused> {
        Err(EmptyRefused::Unsupported(
            "macOS has no call that takes another process's memory out of RAM, and the VM runs \
             in Apple's Virtualization service, whose task port no other program gets; the \
             guest's free memory still goes back through the balloon"
                .into(),
        ))
    }
}
